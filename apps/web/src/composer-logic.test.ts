import { describe, expect, it } from "vite-plus/test";
import type { ComposerTrigger, ComposerTriggerKind } from "./composer-logic";

import {
  clampCollapsedComposerCursor,
  collapseExpandedComposerCursor,
  detectComposerTrigger as detectCapabilityComposerTrigger,
  expandCollapsedComposerCursor,
  isCollapsedCursorAdjacentToInlineToken,
  parseStandaloneComposerCoddyStudioAction,
  replaceTextRange,
} from "./composer-logic";
import { INLINE_TERMINAL_CONTEXT_PLACEHOLDER } from "./lib/terminalContext";

const allCapabilities = {
  providerSlash: true,
  providerDollarSkill: true,
};
const unsupportedDollarSkillContext = {
  enabledDollarSkillNames: new Set<string>(),
};
const refactorDollarSkillContext = {
  enabledDollarSkillNames: new Set(["refactor"]),
};

function detectComposerTrigger(text: string, cursor: number) {
  return detectCapabilityComposerTrigger(text, cursor, allCapabilities);
}

describe("detectComposerTrigger", () => {
  it("exports the canonical trigger type", () => {
    const canonicalKind: ComposerTriggerKind = "coddy-studio-action";
    const canonical = {
      kind: canonicalKind,
      query: "plan",
      rangeStart: 0,
      rangeEnd: 5,
    } satisfies ComposerTrigger;

    expect(canonical.kind).toBe("coddy-studio-action");
  });

  it("detects @path trigger at cursor", () => {
    const text = "Please check @src/com";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-reference",
      query: "src/com",
      rangeStart: "Please check ".length,
      rangeEnd: text.length,
    });
  });

  it("detects slash command token while typing command name", () => {
    const text = "/mo";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-slash",
      query: "mo",
      rangeStart: 0,
      rangeEnd: text.length,
    });
  });

  it("keeps /model as a slash command item", () => {
    const text = "/model";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-slash",
      query: "model",
      rangeStart: 0,
      rangeEnd: text.length,
    });
  });

  it("does not keep a subcommand trigger active after /model arguments", () => {
    const text = "/model spark";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toBeNull();
  });

  it("detects non-model slash commands while typing", () => {
    const text = "/pl";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-slash",
      query: "pl",
      rangeStart: 0,
      rangeEnd: text.length,
    });
  });

  it("keeps slash command detection active for provider commands", () => {
    const text = "/rev";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-slash",
      query: "rev",
      rangeStart: 0,
      rangeEnd: text.length,
    });
  });

  it("detects $skill trigger at cursor", () => {
    const text = "Use $gh-fi";
    const trigger = detectComposerTrigger(text, text.length);

    expect(trigger).toEqual({
      kind: "provider-dollar-skill",
      query: "gh-fi",
      rangeStart: "Use ".length,
      rangeEnd: text.length,
    });
  });

  it("detects @path trigger in the middle of existing text", () => {
    // User typed @ between "inspect " and "in this sentence"
    const text = "Please inspect @in this sentence";
    const cursorAfterAt = "Please inspect @".length;

    const trigger = detectComposerTrigger(text, cursorAfterAt);
    expect(trigger).toEqual({
      kind: "provider-reference",
      query: "",
      rangeStart: "Please inspect ".length,
      rangeEnd: cursorAfterAt,
    });
  });

  it("detects @path trigger with query typed mid-text", () => {
    // User typed @sr between "inspect " and "in this sentence"
    const text = "Please inspect @srin this sentence";
    const cursorAfterQuery = "Please inspect @sr".length;

    const trigger = detectComposerTrigger(text, cursorAfterQuery);
    expect(trigger).toEqual({
      kind: "provider-reference",
      query: "sr",
      rangeStart: "Please inspect ".length,
      rangeEnd: cursorAfterQuery,
    });
  });

  it("detects trigger with true cursor even when regex-based mention detection would false-match", () => {
    // MENTION_TOKEN_REGEX can false-match plain text like "@in" as a mention.
    // The fix bypasses it by computing the expanded cursor from the Lexical node tree.
    const text = "Please inspect @in this sentence";
    const cursorAfterAt = "Please inspect @".length;

    const trigger = detectComposerTrigger(text, cursorAfterAt);
    expect(trigger).not.toBeNull();
    expect(trigger?.kind).toBe("provider-reference");
    expect(trigger?.query).toBe("");
  });

  it("clamps invalid cursors and treats every supported separator as a token boundary", () => {
    expect(detectComposerTrigger("prefix\n/value", Number.POSITIVE_INFINITY)).toMatchObject({
      kind: "provider-slash",
      rangeStart: "prefix\n".length,
    });
    for (const separator of ["\t", "\r", INLINE_TERMINAL_CONTEXT_PLACEHOLDER]) {
      const text = `before${separator}@path`;
      expect(detectComposerTrigger(text, text.length)).toMatchObject({
        kind: "provider-reference",
        query: "path",
        rangeStart: `before${separator}`.length,
      });
    }
    expect(detectComposerTrigger("@path", -10)).toBeNull();
  });

  it("keeps terminal placeholders as web token boundaries when provider triggers are gated", () => {
    const profile = { providerSlash: false, providerDollarSkill: false };
    const text = `before${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}@path`;

    expect(detectCapabilityComposerTrigger(text, text.length, profile)).toMatchObject({
      kind: "provider-reference",
      query: "path",
      rangeStart: `before${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`.length,
    });
    expect(detectCapabilityComposerTrigger("/review", 7, profile)).toBeNull();
    expect(detectCapabilityComposerTrigger("$review", 7, profile)).toBeNull();
    expect(detectCapabilityComposerTrigger(":plan", 5, profile)?.kind).toBe("coddy-studio-action");
  });

  it("detects Coddy Studio actions independently of provider capabilities", () => {
    expect(
      detectCapabilityComposerTrigger(":plan", 5, {
        providerSlash: false,
        providerDollarSkill: false,
      })?.kind,
    ).toBe("coddy-studio-action");
  });
});

describe("replaceTextRange", () => {
  it("replaces a text range and returns new cursor", () => {
    const replaced = replaceTextRange("hello @src", 6, 10, "");
    expect(replaced).toEqual({
      text: "hello ",
      cursor: 6,
    });
  });

  it("clamps reversed and out-of-range boundaries", () => {
    expect(replaceTextRange("hello", -5, 99, "x")).toEqual({ text: "x", cursor: 1 });
    expect(replaceTextRange("hello", 4, 2, "x")).toEqual({ text: "hellxo", cursor: 5 });
  });
});

describe("expandCollapsedComposerCursor", () => {
  it("keeps cursor unchanged when no mention segment is present", () => {
    expect(expandCollapsedComposerCursor("plain text", 5)).toBe(5);
  });

  it("maps collapsed mention cursor to expanded text cursor", () => {
    const text = "what's in my @AGENTS.md fsfdas";
    const collapsedCursorAfterMention = "what's in my ".length + 2;
    const expandedCursorAfterMention = "what's in my @AGENTS.md ".length;

    expect(expandCollapsedComposerCursor(text, collapsedCursorAfterMention)).toBe(
      expandedCursorAfterMention,
    );
  });

  it("maps collapsed quoted mention cursor to expanded text cursor", () => {
    const text = 'what is in @"My File.md" please';
    const collapsedCursorAfterMention = "what is in ".length + 2;
    const expandedCursorAfterMention = 'what is in @"My File.md" '.length;

    expect(expandCollapsedComposerCursor(text, collapsedCursorAfterMention)).toBe(
      expandedCursorAfterMention,
    );
  });

  it("maps collapsed markdown file links to their expanded source offsets", () => {
    const text = "what's in [AGENTS.md](AGENTS.md) please";
    const collapsedCursorAfterMention = "what's in ".length + 2;
    const expandedCursorAfterMention = "what's in [AGENTS.md](AGENTS.md) ".length;

    expect(expandCollapsedComposerCursor(text, collapsedCursorAfterMention)).toBe(
      expandedCursorAfterMention,
    );
  });

  it("allows path trigger detection to close after selecting a mention", () => {
    const text = "what's in my @AGENTS.md ";
    const collapsedCursorAfterMention = "what's in my ".length + 2;
    const expandedCursor = expandCollapsedComposerCursor(text, collapsedCursorAfterMention);

    expect(detectComposerTrigger(text, expandedCursor)).toBeNull();
  });

  it("maps collapsed skill cursor to expanded text cursor", () => {
    const text = "run $refactor then";
    const collapsedCursorAfterSkill = "run ".length + 2;
    const expandedCursorAfterSkill = "run $refactor ".length;

    expect(
      expandCollapsedComposerCursor(text, collapsedCursorAfterSkill, refactorDollarSkillContext),
    ).toBe(expandedCursorAfterSkill);
  });

  it("keeps unsupported dollar text in identity cursor space", () => {
    const text = "use $refactor now";

    expect(clampCollapsedComposerCursor(text, text.length, unsupportedDollarSkillContext)).toBe(
      text.length,
    );
    expect(collapseExpandedComposerCursor(text, text.length, unsupportedDollarSkillContext)).toBe(
      text.length,
    );
    expect(expandCollapsedComposerCursor(text, text.length, unsupportedDollarSkillContext)).toBe(
      text.length,
    );
  });

  it("maps mentionable agent tokens as one collapsed cursor position", () => {
    const text = "ask @reviewer next";
    const mentionableAgentNames = new Set(["reviewer"]);
    const collapsedCursorAfterAgent = "ask ".length + 2;
    const expandedCursorAfterAgent = "ask @reviewer ".length;

    expect(
      expandCollapsedComposerCursor(text, collapsedCursorAfterAgent, {
        mentionableAgentNames,
      }),
    ).toBe(expandedCursorAfterAgent);
    expect(
      collapseExpandedComposerCursor(text, expandedCursorAfterAgent, {
        mentionableAgentNames,
      }),
    ).toBe(collapsedCursorAfterAgent);
    expect(
      clampCollapsedComposerCursor(text, text.length, {
        mentionableAgentNames,
      }),
    ).toBe("ask ".length + 1 + " next".length);
  });

  it("maps token boundaries, terminal placeholders, and non-finite cursors", () => {
    const mention = "x @AGENTS.md ";
    const skill = "x $review-follow-up ";
    expect(expandCollapsedComposerCursor("", Number.NaN)).toBe(0);
    expect(expandCollapsedComposerCursor(mention, 2)).toBe(2);
    expect(expandCollapsedComposerCursor(mention, 3)).toBe("x @AGENTS.md".length);
    expect(expandCollapsedComposerCursor(skill, 2)).toBe(2);
    expect(expandCollapsedComposerCursor(skill, 3)).toBe("x $review-follow-up".length);
    expect(expandCollapsedComposerCursor(INLINE_TERMINAL_CONTEXT_PLACEHOLDER, 0)).toBe(0);
    expect(expandCollapsedComposerCursor(INLINE_TERMINAL_CONTEXT_PLACEHOLDER, 1)).toBe(1);
    expect(expandCollapsedComposerCursor("plain", Number.POSITIVE_INFINITY)).toBe(5);
  });

  it("maps native references across terminal placeholder delimiters", () => {
    const text = `open @src/a.ts${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}@src/b.ts `;

    expect(expandCollapsedComposerCursor(text, 6)).toBe("open @src/a.ts".length);
    expect(expandCollapsedComposerCursor(text, 7)).toBe(
      `open @src/a.ts${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`.length,
    );
    expect(expandCollapsedComposerCursor(text, 8)).toBe(text.length - 1);
    expect(collapseExpandedComposerCursor(text, "open @src/a.ts".length)).toBe(6);
    expect(
      collapseExpandedComposerCursor(
        text,
        `open @src/a.ts${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`.length,
      ),
    ).toBe(7);
    expect(collapseExpandedComposerCursor(text, text.length - 1)).toBe(8);
    expect(clampCollapsedComposerCursor(text, Number.POSITIVE_INFINITY)).toBe(9);
    expect(isCollapsedCursorAdjacentToInlineToken(text, 6, "left")).toBe(true);
    expect(isCollapsedCursorAdjacentToInlineToken(text, 7, "right")).toBe(true);
  });
});

describe("collapseExpandedComposerCursor", () => {
  it("keeps cursor unchanged when no mention segment is present", () => {
    expect(collapseExpandedComposerCursor("plain text", 5)).toBe(5);
  });

  it("maps expanded mention cursor back to collapsed cursor", () => {
    const text = "what's in my @AGENTS.md fsfdas";
    const collapsedCursorAfterMention = "what's in my ".length + 2;
    const expandedCursorAfterMention = "what's in my @AGENTS.md ".length;

    expect(collapseExpandedComposerCursor(text, expandedCursorAfterMention)).toBe(
      collapsedCursorAfterMention,
    );
  });

  it("maps expanded quoted mention cursor back to collapsed cursor", () => {
    const text = 'what is in @"My File.md" please';
    const collapsedCursorAfterMention = "what is in ".length + 2;
    const expandedCursorAfterMention = 'what is in @"My File.md" '.length;

    expect(collapseExpandedComposerCursor(text, expandedCursorAfterMention)).toBe(
      collapsedCursorAfterMention,
    );
  });

  it("maps expanded markdown file link cursors back to collapsed offsets", () => {
    const text = "what's in [AGENTS.md](AGENTS.md) please";
    const collapsedCursorAfterMention = "what's in ".length + 2;
    const expandedCursorAfterMention = "what's in [AGENTS.md](AGENTS.md) ".length;

    expect(collapseExpandedComposerCursor(text, expandedCursorAfterMention)).toBe(
      collapsedCursorAfterMention,
    );
  });

  it("keeps replacement cursors aligned when another mention already exists earlier", () => {
    const text = "open @AGENTS.md then @src/index.ts ";
    const expandedCursor = text.length;
    const collapsedCursor = collapseExpandedComposerCursor(text, expandedCursor);

    expect(collapsedCursor).toBe("open ".length + 1 + " then ".length + 2);
    expect(expandCollapsedComposerCursor(text, collapsedCursor)).toBe(expandedCursor);
  });

  it("maps expanded skill cursor back to collapsed cursor", () => {
    const text = "run $refactor then";
    const collapsedCursorAfterSkill = "run ".length + 2;
    const expandedCursorAfterSkill = "run $refactor ".length;

    expect(
      collapseExpandedComposerCursor(text, expandedCursorAfterSkill, refactorDollarSkillContext),
    ).toBe(collapsedCursorAfterSkill);
  });

  it("collapses positions inside each inline token type", () => {
    const mention = "x @AGENTS.md ";
    const skill = "x $review-follow-up ";
    expect(collapseExpandedComposerCursor("", Number.NaN)).toBe(0);
    expect(collapseExpandedComposerCursor(mention, 2)).toBe(2);
    expect(collapseExpandedComposerCursor(mention, 3)).toBe(3);
    expect(collapseExpandedComposerCursor(mention, "x @AGENTS.md".length)).toBe(3);
    expect(collapseExpandedComposerCursor(skill, 2)).toBe(2);
    expect(collapseExpandedComposerCursor(skill, 4)).toBe(3);
    expect(collapseExpandedComposerCursor(INLINE_TERMINAL_CONTEXT_PLACEHOLDER, 0)).toBe(0);
    expect(collapseExpandedComposerCursor(INLINE_TERMINAL_CONTEXT_PLACEHOLDER, 1)).toBe(1);
  });
});

describe("clampCollapsedComposerCursor", () => {
  it("clamps to collapsed prompt length when mentions are present", () => {
    const text = "open @AGENTS.md then ";

    expect(clampCollapsedComposerCursor(text, text.length)).toBe(
      "open ".length + 1 + " then ".length,
    );
    expect(clampCollapsedComposerCursor(text, Number.POSITIVE_INFINITY)).toBe(
      "open ".length + 1 + " then ".length,
    );
  });
});

describe("replaceTextRange trailing space consumption", () => {
  it("double space after insertion when replacement ends with space", () => {
    // Simulates: "and then |@AG| summarize" where | marks replacement range
    // The replacement is "@AGENTS.md " (with trailing space)
    // But if we don't extend rangeEnd, the existing space stays
    const text = "and then @AG summarize";
    const rangeStart = "and then ".length;
    const rangeEnd = "and then @AG".length;

    // Without consuming trailing space: double space
    const withoutConsume = replaceTextRange(text, rangeStart, rangeEnd, "@AGENTS.md ");
    expect(withoutConsume.text).toBe("and then @AGENTS.md  summarize");

    // With consuming trailing space: single space
    const extendedEnd = text[rangeEnd] === " " ? rangeEnd + 1 : rangeEnd;
    const withConsume = replaceTextRange(text, rangeStart, extendedEnd, "@AGENTS.md ");
    expect(withConsume.text).toBe("and then @AGENTS.md summarize");
  });
});

describe("isCollapsedCursorAdjacentToInlineToken", () => {
  it("returns false when no mention exists", () => {
    expect(isCollapsedCursorAdjacentToInlineToken("plain text", 6, "left")).toBe(false);
    expect(isCollapsedCursorAdjacentToInlineToken("plain text", 6, "right")).toBe(false);
  });

  it("keeps @query typing non-adjacent while no mention pill exists", () => {
    const text = "hello @pac";
    expect(isCollapsedCursorAdjacentToInlineToken(text, text.length, "left")).toBe(false);
    expect(isCollapsedCursorAdjacentToInlineToken(text, text.length, "right")).toBe(false);
    expect(clampCollapsedComposerCursor(text, text.length)).toBe(text.length);
    expect(detectComposerTrigger(text, text.length)).toMatchObject({
      kind: "provider-reference",
      query: "pac",
    });
  });

  it("detects left adjacency only when cursor is directly after a mention", () => {
    const text = "open @AGENTS.md next";
    const mentionStart = "open ".length;
    const mentionEnd = mentionStart + 1;

    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionEnd, "left")).toBe(true);
    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionStart, "left")).toBe(false);
    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionEnd + 1, "left")).toBe(false);
  });

  it("detects right adjacency only when cursor is directly before a mention", () => {
    const text = "open @AGENTS.md next";
    const mentionStart = "open ".length;
    const mentionEnd = mentionStart + 1;

    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionStart, "right")).toBe(true);
    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionEnd, "right")).toBe(false);
    expect(isCollapsedCursorAdjacentToInlineToken(text, mentionStart - 1, "right")).toBe(false);
  });

  it("treats terminal pills as inline tokens for adjacency checks", () => {
    const text = `open ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} next`;
    const tokenStart = "open ".length;
    const tokenEnd = tokenStart + 1;

    expect(isCollapsedCursorAdjacentToInlineToken(text, tokenEnd, "left")).toBe(true);
    expect(isCollapsedCursorAdjacentToInlineToken(text, tokenStart, "right")).toBe(true);
  });

  it("treats skill pills as inline tokens for adjacency checks", () => {
    const text = "run $refactor next";
    const tokenStart = "run ".length;
    const tokenEnd = tokenStart + 1;

    expect(
      isCollapsedCursorAdjacentToInlineToken(text, tokenEnd, "left", refactorDollarSkillContext),
    ).toBe(true);
    expect(
      isCollapsedCursorAdjacentToInlineToken(text, tokenStart, "right", refactorDollarSkillContext),
    ).toBe(true);
  });

  it("keeps unsupported dollar text non-adjacent like ordinary text", () => {
    const text = "use $refactor now";
    const dollarStart = "use ".length;
    const dollarEnd = "use $refactor".length;

    expect(
      isCollapsedCursorAdjacentToInlineToken(
        text,
        dollarStart,
        "right",
        unsupportedDollarSkillContext,
      ),
    ).toBe(false);
    expect(
      isCollapsedCursorAdjacentToInlineToken(
        text,
        dollarEnd,
        "left",
        unsupportedDollarSkillContext,
      ),
    ).toBe(false);
    expect(
      isCollapsedCursorAdjacentToInlineToken(
        text,
        text.length,
        "left",
        unsupportedDollarSkillContext,
      ),
    ).toBe(false);
  });

  it("treats mentionable agent pills as inline tokens for adjacency checks", () => {
    const text = "ask @reviewer next";
    const mentionableAgentNames = new Set(["reviewer"]);
    const tokenStart = "ask ".length;
    const tokenEnd = tokenStart + 1;

    expect(
      isCollapsedCursorAdjacentToInlineToken(text, tokenEnd, "left", {
        mentionableAgentNames,
      }),
    ).toBe(true);
    expect(
      isCollapsedCursorAdjacentToInlineToken(text, tokenStart, "right", {
        mentionableAgentNames,
      }),
    ).toBe(true);
  });
});

describe("parseStandaloneComposerCoddyStudioAction", () => {
  it("parses standalone :plan action", () => {
    expect(parseStandaloneComposerCoddyStudioAction(" :plan ")).toBe("plan");
  });

  it("parses standalone :default action", () => {
    expect(parseStandaloneComposerCoddyStudioAction(":default")).toBe("default");
  });

  it("ignores actions with extra message text", () => {
    expect(parseStandaloneComposerCoddyStudioAction(":plan explain this")).toBeNull();
  });
});
