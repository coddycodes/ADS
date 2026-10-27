import { ThreadId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  appendTerminalContextsToPrompt,
  buildTerminalContextPreviewTitle,
  buildTerminalContextBlock,
  countInlineTerminalContextPlaceholders,
  deriveDisplayedUserMessageState,
  ensureInlineTerminalContextPlaceholders,
  extractTrailingTerminalContexts,
  filterTerminalContextsWithText,
  formatInlineTerminalContextLabel,
  formatTerminalContextLabel,
  hasTerminalContextText,
  INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
  insertInlineTerminalContextPlaceholder,
  isTerminalContextExpired,
  materializeInlineTerminalContextPrompt,
  removeInlineTerminalContextPlaceholder,
  stripInlineTerminalContextPlaceholders,
  type TerminalContextDraft,
} from "./terminalContext";

function makeContext(overrides?: Partial<TerminalContextDraft>): TerminalContextDraft {
  return {
    id: "context-1",
    threadId: ThreadId.make("thread-1"),
    terminalId: "default",
    terminalLabel: "Terminal 1",
    lineStart: 12,
    lineEnd: 13,
    text: "git status\nOn branch main",
    createdAt: "2026-03-13T12:00:00.000Z",
    ...overrides,
  };
}

describe("terminalContext", () => {
  it("formats terminal labels with line ranges", () => {
    expect(formatTerminalContextLabel(makeContext())).toBe("Terminal 1 lines 12-13");
    expect(
      formatTerminalContextLabel(
        makeContext({
          lineStart: 9,
          lineEnd: 9,
        }),
      ),
    ).toBe("Terminal 1 line 9");
  });

  it("builds a numbered terminal context block", () => {
    expect(buildTerminalContextBlock([makeContext()])).toBe(
      [
        "<terminal_context>",
        "- Terminal 1 lines 12-13:",
        "  12 | git status",
        "  13 | On branch main",
        "</terminal_context>",
      ].join("\n"),
    );
  });

  it("appends terminal context blocks after prompt text", () => {
    expect(appendTerminalContextsToPrompt("Investigate this", [makeContext()])).toBe(
      [
        "Investigate this",
        "",
        "<terminal_context>",
        "- Terminal 1 lines 12-13:",
        "  12 | git status",
        "  13 | On branch main",
        "</terminal_context>",
      ].join("\n"),
    );
  });

  it("replaces inline placeholders with inline terminal labels before appending context blocks", () => {
    expect(
      appendTerminalContextsToPrompt(
        `Investigate ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} carefully`,
        [makeContext()],
      ),
    ).toBe(
      [
        "Investigate @terminal-1:12-13 carefully",
        "",
        "<terminal_context>",
        "- Terminal 1 lines 12-13:",
        "  12 | git status",
        "  13 | On branch main",
        "</terminal_context>",
      ].join("\n"),
    );
  });

  it("extracts terminal context blocks from message text", () => {
    const prompt = appendTerminalContextsToPrompt("Investigate this", [makeContext()]);
    expect(extractTrailingTerminalContexts(prompt)).toEqual({
      promptText: "Investigate this",
      contextCount: 1,
      previewTitle: "Terminal 1 lines 12-13\n12 | git status\n13 | On branch main",
      contexts: [
        {
          header: "Terminal 1 lines 12-13",
          body: "12 | git status\n13 | On branch main",
        },
      ],
    });
  });

  it("derives displayed user message state from terminal context prompts", () => {
    const prompt = appendTerminalContextsToPrompt("Investigate this", [makeContext()]);
    expect(deriveDisplayedUserMessageState(prompt)).toEqual({
      visibleText: "Investigate this",
      copyText: prompt,
      contextCount: 1,
      previewTitle: "Terminal 1 lines 12-13\n12 | git status\n13 | On branch main",
      contexts: [
        {
          header: "Terminal 1 lines 12-13",
          body: "12 | git status\n13 | On branch main",
        },
      ],
      elementContexts: [],
    });
  });

  it("preserves prompt text when no trailing terminal context block exists", () => {
    expect(extractTrailingTerminalContexts("No attached context")).toEqual({
      promptText: "No attached context",
      contextCount: 0,
      previewTitle: null,
      contexts: [],
    });
  });

  it("returns null preview title when every context is invalid", () => {
    expect(
      buildTerminalContextPreviewTitle([
        makeContext({
          terminalId: "   ",
        }),
        makeContext({
          id: "context-2",
          text: "\n\n",
        }),
      ]),
    ).toBeNull();
  });

  it("tracks inline terminal context placeholders in prompt text", () => {
    const placeholder = INLINE_TERMINAL_CONTEXT_PLACEHOLDER;
    expect(countInlineTerminalContextPlaceholders(`a${placeholder}b${placeholder}`)).toBe(2);
    expect(ensureInlineTerminalContextPlaceholders("Investigate this", 2)).toBe(
      `${placeholder}${placeholder}Investigate this`,
    );
    expect(insertInlineTerminalContextPlaceholder("abc", 1)).toEqual({
      prompt: `a ${placeholder} bc`,
      cursor: 4,
      contextIndex: 0,
    });
    expect(removeInlineTerminalContextPlaceholder(`a${placeholder}b${placeholder}c`, 1)).toEqual({
      prompt: `a${placeholder}bc`,
      cursor: 3,
    });
    expect(stripInlineTerminalContextPlaceholders(`a${placeholder}b`)).toBe("ab");
  });

  it("inserts a placeholder after a file mention when given the expanded prompt cursor", () => {
    const placeholder = INLINE_TERMINAL_CONTEXT_PLACEHOLDER;
    expect(insertInlineTerminalContextPlaceholder("Inspect @package.json ", 22)).toEqual({
      prompt: `Inspect @package.json ${placeholder} `,
      cursor: 24,
      contextIndex: 0,
    });
  });

  it("adds a trailing space and consumes an existing trailing space at the insertion point", () => {
    const placeholder = INLINE_TERMINAL_CONTEXT_PLACEHOLDER;
    expect(insertInlineTerminalContextPlaceholder("yo whats", 3)).toEqual({
      prompt: `yo ${placeholder} whats`,
      cursor: 5,
      contextIndex: 0,
    });
  });

  it("marks contexts without snapshot text as expired and filters them from sendable contexts", () => {
    const liveContext = makeContext();
    const expiredContext = makeContext({
      id: "context-2",
      text: "",
    });

    expect(hasTerminalContextText(liveContext)).toBe(true);
    expect(isTerminalContextExpired(liveContext)).toBe(false);
    expect(hasTerminalContextText(expiredContext)).toBe(false);
    expect(isTerminalContextExpired(expiredContext)).toBe(true);
    expect(filterTerminalContextsWithText([expiredContext, liveContext])).toEqual([liveContext]);
  });

  it("formats and materializes inline terminal labels from placeholder positions", () => {
    expect(formatInlineTerminalContextLabel(makeContext())).toBe("@terminal-1:12-13");
    expect(
      materializeInlineTerminalContextPrompt(
        `Investigate ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} carefully`,
        [makeContext()],
      ),
    ).toBe("Investigate @terminal-1:12-13 carefully");
  });

  it("builds short, multiline, and truncated context previews", () => {
    expect(buildTerminalContextPreviewTitle([])).toBeNull();
    expect(buildTerminalContextPreviewTitle([makeContext({ text: "one" })])).toContain("one");
    expect(
      buildTerminalContextPreviewTitle([makeContext({ text: "one\ntwo\nthree\nfour" })]),
    ).toContain("...");
    expect(buildTerminalContextPreviewTitle([makeContext({ text: "x".repeat(220) })])).toContain(
      `${"x".repeat(177)}...`,
    );
  });

  it("normalizes invalid selections and separates multiple context blocks", () => {
    expect(buildTerminalContextBlock([])).toBe("");
    expect(buildTerminalContextBlock([makeContext({ terminalLabel: " " })])).toBe("");
    const block = buildTerminalContextBlock([
      makeContext({ id: "first", lineStart: 0.9, lineEnd: 0.2, text: "\r\nfirst\r\n" }),
      makeContext({ id: "second", terminalId: " second ", terminalLabel: " Two ", text: "next" }),
    ]);
    expect(block).toContain("Terminal 1 line 1");
    expect(block).toContain("\n\n- Two lines 12-13:");
  });

  it("drops surplus inline placeholders and handles empty prompt/context combinations", () => {
    expect(
      materializeInlineTerminalContextPrompt(
        `${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}x${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`,
        [makeContext()],
      ),
    ).toBe("@terminal-1:12-13x");
    expect(appendTerminalContextsToPrompt("  prompt  ", [])).toBe("prompt");
    expect(appendTerminalContextsToPrompt("", [makeContext()])).toBe(
      buildTerminalContextBlock([makeContext()]),
    );
  });

  it("parses headers without bodies and ignores unrelated block lines", () => {
    expect(
      extractTrailingTerminalContexts(
        "Prompt\n<terminal_context>\nnoise\n- Terminal 1 line 1:\n</terminal_context>",
      ),
    ).toMatchObject({
      promptText: "Prompt",
      contextCount: 1,
      previewTitle: "Terminal 1 line 1",
      contexts: [{ header: "Terminal 1 line 1", body: "" }],
    });
    expect(
      extractTrailingTerminalContexts("Prompt\n<terminal_context>\nnoise\n</terminal_context>"),
    ).toMatchObject({ contextCount: 0, previewTitle: null, contexts: [] });
  });

  it("handles negative and out-of-range placeholder removal", () => {
    const prompt = `a${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}b`;
    expect(removeInlineTerminalContextPlaceholder(prompt, -1)).toEqual({
      prompt,
      cursor: prompt.length,
    });
    expect(removeInlineTerminalContextPlaceholder(prompt, 5)).toEqual({
      prompt,
      cursor: prompt.length,
    });
    expect(ensureInlineTerminalContextPlaceholders(prompt, 1)).toBe(prompt);
  });
});
