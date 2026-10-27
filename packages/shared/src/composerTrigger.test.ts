import { describe, expect, it } from "vite-plus/test";

import {
  detectComposerTrigger,
  parseStandaloneComposerCoddyStudioAction,
  replaceTextRange,
  serializeComposerFileLink,
  serializeComposerMentionPath,
} from "./composerTrigger.ts";

describe("serializeComposerMentionPath compatibility re-export", () => {
  it("keeps simple mention paths unquoted", () => {
    expect(serializeComposerMentionPath("src/index.ts")).toBe("src/index.ts");
  });

  it("quotes mention paths containing whitespace", () => {
    expect(serializeComposerMentionPath("docs/My File.md")).toBe('"docs/My File.md"');
  });

  it("escapes quoted mention path content", () => {
    expect(serializeComposerMentionPath('docs\\My "File".md')).toBe('"docs\\\\My \\"File\\".md"');
  });
});

describe("serializeComposerFileLink", () => {
  it("uses the basename as the markdown label", () => {
    expect(serializeComposerFileLink("path/to/package.json")).toBe(
      "[package.json](path/to/package.json)",
    );
  });

  it("encodes markdown-sensitive destination characters", () => {
    expect(serializeComposerFileLink("docs/My File (draft).md")).toBe(
      "[My File (draft).md](docs/My%20File%20%28draft%29.md)",
    );
  });

  it("supports windows paths", () => {
    expect(serializeComposerFileLink("C:\\repo\\src\\index.ts")).toBe(
      "[index.ts](C:%5Crepo%5Csrc%5Cindex.ts)",
    );
  });

  it("preserves paths that legitimately start with an at sign", () => {
    expect(serializeComposerFileLink("@scope/package.json")).toBe(
      "[package.json](@scope/package.json)",
    );
  });

  it("escapes markdown labels and every reserved destination character", () => {
    expect(serializeComposerFileLink("folder\\a[b]#c?.txt")).toBe(
      "[a\\[b\\]#c?.txt](folder%5Ca%5Bb%5D%23c%3F.txt)",
    );
  });

  it("uses the whole path as the label when it has no separator", () => {
    expect(serializeComposerFileLink("README.md")).toBe("[README.md](README.md)");
  });
});

describe("detectComposerTrigger", () => {
  const allCapabilities = {
    providerSlash: true,
    providerDollarSkill: true,
  };

  it.each([
    [":mod", "coddy-studio-action"],
    ["/rev", "provider-slash"],
    ["use $ref", "provider-dollar-skill"],
    ["open @src", "provider-reference"],
  ] as const)("detects %s as %s", (text, kind) => {
    expect(detectComposerTrigger(text, text.length, allCapabilities)).toMatchObject({ kind });
  });

  it("leaves unsupported provider triggers as text", () => {
    const profile = { providerSlash: false, providerDollarSkill: false };
    expect(detectComposerTrigger("/review", 7, profile)).toBeNull();
    expect(detectComposerTrigger("$review", 7, profile)).toBeNull();
    expect(detectComposerTrigger("@src", 4, profile)?.kind).toBe("provider-reference");
    expect(detectComposerTrigger(":plan", 5, profile)?.kind).toBe("coddy-studio-action");
  });

  it("detects colon and slash triggers only as the current line's single token", () => {
    expect(detectComposerTrigger(":plan", 5, allCapabilities)).toEqual({
      kind: "coddy-studio-action",
      query: "plan",
      rangeStart: 0,
      rangeEnd: 5,
    });
    expect(detectComposerTrigger("/review", 7, allCapabilities)).toEqual({
      kind: "provider-slash",
      query: "review",
      rangeStart: 0,
      rangeEnd: 7,
    });
    expect(detectComposerTrigger("prefix :plan", 12, allCapabilities)).toBeNull();
    expect(detectComposerTrigger("prefix /review", 14, allCapabilities)).toBeNull();
    expect(detectComposerTrigger(":plan now", 9, allCapabilities)).toBeNull();
    expect(detectComposerTrigger("/review now", 11, allCapabilities)).toBeNull();
  });

  it("supports line-start triggers after newlines", () => {
    expect(detectComposerTrigger("prefix\n:plan", 12, allCapabilities)).toMatchObject({
      kind: "coddy-studio-action",
      rangeStart: 7,
      rangeEnd: 12,
    });
    expect(detectComposerTrigger("prefix\n/review", 14, allCapabilities)).toMatchObject({
      kind: "provider-slash",
      rangeStart: 7,
      rangeEnd: 14,
    });
  });

  it("detects references and dollar skills after supported whitespace boundaries", () => {
    expect(detectComposerTrigger("open @src/app", 13, allCapabilities)).toEqual({
      kind: "provider-reference",
      query: "src/app",
      rangeStart: 5,
      rangeEnd: 13,
    });
    expect(detectComposerTrigger("use\t$review", 11, allCapabilities)).toEqual({
      kind: "provider-dollar-skill",
      query: "review",
      rangeStart: 4,
      rangeEnd: 11,
    });
    expect(detectComposerTrigger("use\r@file", 9, allCapabilities)?.query).toBe("file");
    expect(detectComposerTrigger("use\n$", 5, allCapabilities)?.query).toBe("");
  });

  it("supports caller-defined token boundaries", () => {
    expect(
      detectComposerTrigger("context\u0000@src", 12, allCapabilities, {
        isWhitespaceChar: (char) => char === "\u0000",
      }),
    ).toEqual({
      kind: "provider-reference",
      query: "src",
      rangeStart: 8,
      rangeEnd: 12,
    });
  });

  it("clamps invalid and out-of-range cursor positions", () => {
    expect(detectComposerTrigger("@file", Number.NaN, allCapabilities)?.rangeEnd).toBe(5);
    expect(
      detectComposerTrigger("@file", Number.POSITIVE_INFINITY, allCapabilities)?.rangeEnd,
    ).toBe(5);
    expect(detectComposerTrigger("@file", 99, allCapabilities)?.rangeEnd).toBe(5);
    expect(detectComposerTrigger("@file", 3.9, allCapabilities)).toEqual({
      kind: "provider-reference",
      query: "fi",
      rangeStart: 0,
      rangeEnd: 3,
    });
    expect(detectComposerTrigger("@file", -2, allCapabilities)).toBeNull();
  });

  it("returns null for ordinary and empty tokens", () => {
    expect(detectComposerTrigger("ordinary", 8, allCapabilities)).toBeNull();
    expect(detectComposerTrigger("", 0, allCapabilities)).toBeNull();
  });
});

describe("parseStandaloneComposerCoddyStudioAction", () => {
  it("parses standalone actions case-insensitively", () => {
    expect(parseStandaloneComposerCoddyStudioAction("  :MODEL  ")).toBe("model");
    expect(parseStandaloneComposerCoddyStudioAction(":PLAN")).toBe("plan");
    expect(parseStandaloneComposerCoddyStudioAction(":Default\n")).toBe("default");
  });

  it("rejects arguments and unrelated text", () => {
    expect(parseStandaloneComposerCoddyStudioAction(":plan now")).toBeNull();
    expect(parseStandaloneComposerCoddyStudioAction("/plan")).toBeNull();
    expect(parseStandaloneComposerCoddyStudioAction("plan")).toBeNull();
  });
});

describe("replaceTextRange", () => {
  it("replaces a bounded range and reports the next cursor", () => {
    expect(replaceTextRange("hello world", 6, 11, "there")).toEqual({
      text: "hello there",
      cursor: 11,
    });
  });

  it("clamps reversed and out-of-range boundaries", () => {
    expect(replaceTextRange("abc", -10, -1, "x")).toEqual({ text: "xabc", cursor: 1 });
    expect(replaceTextRange("abc", 2, 1, "x")).toEqual({ text: "abxc", cursor: 3 });
    expect(replaceTextRange("abc", 20, 30, "x")).toEqual({ text: "abcx", cursor: 4 });
  });
});
