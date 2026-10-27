import {
  INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
  type TerminalContextDraft,
} from "./lib/terminalContext";
import {
  collectComposerInlineTokens,
  type ComposerInlineToken,
} from "@coddy/shared/composerInlineTokens";

export type ComposerPromptSegment =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "mention";
      path: string;
      source: string;
    }
  | {
      type: "agent";
      name: string;
      source: string;
    }
  | {
      type: "skill";
      name: string;
    }
  | {
      type: "terminal-context";
      context: TerminalContextDraft | null;
    };

export interface SplitPromptIntoComposerSegmentsOptions {
  readonly reconstructTrailingReferences?: boolean;
  /**
   * When provided, only matching names reconstruct as skill segments.
   * Omission preserves provider-agnostic parsing for cursor utilities.
   */
  readonly enabledDollarSkillNames?: ReadonlySet<string>;
}

function rangeIncludesIndex(start: number, end: number, index: number): boolean {
  return start <= index && index < end;
}

function pushTextSegment(segments: ComposerPromptSegment[], text: string): void {
  if (!text) return;
  const last = segments[segments.length - 1];
  if (last && last.type === "text") {
    last.text += text;
    return;
  }
  segments.push({ type: "text", text });
}

function forEachPromptSegmentSlice(
  prompt: string,
  visitor: (
    slice:
      | {
          type: "text";
          text: string;
          promptOffset: number;
          terminatedByTerminalContext: boolean;
        }
      | {
          type: "terminal-context";
          promptOffset: number;
        },
  ) => boolean | void,
): boolean {
  let textCursor = 0;

  for (let index = 0; index < prompt.length; index += 1) {
    if (prompt[index] !== INLINE_TERMINAL_CONTEXT_PLACEHOLDER) {
      continue;
    }

    if (
      index > textCursor &&
      visitor({
        type: "text",
        text: prompt.slice(textCursor, index),
        promptOffset: textCursor,
        terminatedByTerminalContext: true,
      }) === true
    ) {
      return true;
    }
    if (visitor({ type: "terminal-context", promptOffset: index }) === true) {
      return true;
    }
    textCursor = index + 1;
  }

  if (
    textCursor < prompt.length &&
    visitor({
      type: "text",
      text: prompt.slice(textCursor),
      promptOffset: textCursor,
      terminatedByTerminalContext: false,
    }) === true
  ) {
    return true;
  }

  return false;
}

function forEachPromptTextSlice(
  prompt: string,
  visitor: (text: string, promptOffset: number) => boolean | void,
): boolean {
  return forEachPromptSegmentSlice(prompt, (slice) => {
    if (slice.type !== "text") {
      return false;
    }
    return visitor(slice.text, slice.promptOffset);
  });
}

function forEachMentionMatch(
  prompt: string,
  visitor: (
    match: Extract<ComposerInlineToken, { type: "mention" }>,
    promptOffset: number,
  ) => boolean | void,
): boolean {
  return forEachPromptTextSlice(prompt, (text, promptOffset) => {
    for (const match of collectComposerInlineTokens(text)) {
      if (match.type !== "mention") {
        continue;
      }
      if (visitor(match, promptOffset) === true) {
        return true;
      }
    }
    return false;
  });
}

function trailingNativeMentionToken(text: string): ComposerInlineToken | null {
  const match = /(^|\s)@(?:"((?:\\.|[^"\\])*)"|([^\s@"]+))$/.exec(text);
  if (!match) {
    return null;
  }
  const prefix = match[1] ?? "";
  const quotedValue = match[2];
  const value = quotedValue !== undefined ? quotedValue.replace(/\\(.)/g, "$1") : (match[3] ?? "");
  if (!value) {
    return null;
  }
  const start = (match.index ?? 0) + prefix.length;
  return {
    type: "mention",
    value,
    source: text.slice(start),
    start,
    end: text.length,
  };
}

function splitPromptTextIntoComposerSegments(
  text: string,
  mentionableAgentNames: ReadonlySet<string>,
  enabledDollarSkillNames: ReadonlySet<string> | null,
  parseTrailingReference: boolean,
): ComposerPromptSegment[] {
  const segments: ComposerPromptSegment[] = [];
  if (!text) {
    return segments;
  }

  const tokenMatches = [...collectComposerInlineTokens(text)];
  if (parseTrailingReference) {
    const trailingMention = trailingNativeMentionToken(text);
    if (
      trailingMention &&
      !tokenMatches.some(
        (token) =>
          token.type === trailingMention.type &&
          token.start === trailingMention.start &&
          token.end === trailingMention.end,
      )
    ) {
      tokenMatches.push(trailingMention);
      tokenMatches.sort((left, right) => left.start - right.start);
    }
  }
  let cursor = 0;
  for (const match of tokenMatches) {
    if (match.start < cursor) {
      continue;
    }

    if (match.start > cursor) {
      pushTextSegment(segments, text.slice(cursor, match.start));
    }

    if (match.type === "mention") {
      if (match.source.startsWith("@") && mentionableAgentNames.has(match.value)) {
        segments.push({
          type: "agent",
          name: match.value,
          source: match.source,
        });
      } else {
        segments.push({
          type: "mention",
          path: match.value,
          source: match.source,
        });
      }
    } else if (
      enabledDollarSkillNames === null ||
      enabledDollarSkillNames.has(match.value.toLowerCase())
    ) {
      segments.push({ type: "skill", name: match.value });
    } else {
      pushTextSegment(segments, match.source);
    }

    cursor = match.end;
  }

  if (cursor < text.length) {
    pushTextSegment(segments, text.slice(cursor));
  }

  return segments;
}

export function selectionTouchesMentionBoundary(
  prompt: string,
  start: number,
  end: number,
): boolean {
  if (!prompt || start >= end) {
    return false;
  }

  return forEachMentionMatch(prompt, (match, promptOffset) => {
    const mentionStart = promptOffset + match.start;
    const mentionEnd = promptOffset + match.end;
    const beforeMentionIndex = mentionStart - 1;
    const afterMentionIndex = mentionEnd;

    if (
      beforeMentionIndex >= 0 &&
      /\s/.test(prompt[beforeMentionIndex] ?? "") &&
      rangeIncludesIndex(start, end, beforeMentionIndex)
    ) {
      return true;
    }

    if (
      afterMentionIndex < prompt.length &&
      /\s/.test(prompt[afterMentionIndex] ?? "") &&
      rangeIncludesIndex(start, end, afterMentionIndex)
    ) {
      return true;
    }
    return false;
  });
}

export function splitPromptIntoComposerSegments(
  prompt: string,
  terminalContexts: ReadonlyArray<TerminalContextDraft> = [],
  mentionableAgentNames: ReadonlySet<string> = new Set(),
  options: SplitPromptIntoComposerSegmentsOptions = {},
): ComposerPromptSegment[] {
  if (!prompt) {
    return [];
  }

  const segments: ComposerPromptSegment[] = [];
  const enabledDollarSkillNames =
    options.enabledDollarSkillNames === undefined
      ? null
      : new Set([...options.enabledDollarSkillNames].map((name) => name.trim().toLowerCase()));
  let terminalContextIndex = 0;
  forEachPromptSegmentSlice(prompt, (slice) => {
    if (slice.type === "text") {
      segments.push(
        ...splitPromptTextIntoComposerSegments(
          slice.text,
          mentionableAgentNames,
          enabledDollarSkillNames,
          options.reconstructTrailingReferences === true || slice.terminatedByTerminalContext,
        ),
      );
      return false;
    }

    segments.push({
      type: "terminal-context",
      context: terminalContexts[terminalContextIndex] ?? null,
    });
    terminalContextIndex += 1;
    return false;
  });

  return segments;
}
