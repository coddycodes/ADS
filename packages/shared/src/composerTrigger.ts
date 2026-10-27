export { serializeComposerMentionPath } from "./composerReferences.ts";

export type ComposerTriggerKind =
  | "coddy-studio-action"
  | "provider-slash"
  | "provider-dollar-skill"
  | "provider-reference";

export interface ComposerTriggerProfile {
  readonly providerSlash: boolean;
  readonly providerDollarSkill: boolean;
}

export type ComposerCoddyStudioAction = "model" | "plan" | "default";

export interface ComposerTrigger {
  kind: ComposerTriggerKind;
  query: string;
  rangeStart: number;
  rangeEnd: number;
}

function composerFileLinkBasename(path: string): string {
  const separatorIndex = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separatorIndex >= 0 ? path.slice(separatorIndex + 1) : path;
}

function escapeMarkdownLinkLabel(label: string): string {
  return label.replaceAll("\\", "\\\\").replaceAll("[", "\\[").replaceAll("]", "\\]");
}

function encodeMarkdownLinkDestination(path: string): string {
  return encodeURI(path)
    .replaceAll("(", "%28")
    .replaceAll(")", "%29")
    .replaceAll("#", "%23")
    .replaceAll("?", "%3F")
    .replaceAll("\\", "%5C");
}

/** @deprecated Use serializeComposerReference instead. */
export function serializeComposerFileLink(path: string): string {
  const label = escapeMarkdownLinkLabel(composerFileLinkBasename(path));
  return `[${label}](${encodeMarkdownLinkDestination(path)})`;
}

function clampCursor(text: string, cursor: number): number {
  if (!Number.isFinite(cursor)) return text.length;
  return Math.max(0, Math.min(text.length, Math.floor(cursor)));
}

function isWhitespace(char: string): boolean {
  return char === " " || char === "\n" || char === "\t" || char === "\r";
}

/**
 * Detect an active composer action or provider trigger at the cursor position.
 *
 * Accepts an optional `isWhitespaceChar` override so callers with inline
 * placeholder characters (e.g. terminal context chips on web) can treat
 * those as token boundaries.
 */
export function detectComposerTrigger(
  text: string,
  cursorInput: number,
  profile: ComposerTriggerProfile,
  options?: {
    readonly isWhitespaceChar?: (char: string) => boolean;
  },
): ComposerTrigger | null {
  const cursor = clampCursor(text, cursorInput);
  const lineStart = text.lastIndexOf("\n", Math.max(0, cursor - 1)) + 1;
  const linePrefix = text.slice(lineStart, cursor);
  const wsCheck = options?.isWhitespaceChar ?? isWhitespace;

  let linePrefixHasWhitespace = false;
  for (let index = 0; index < linePrefix.length; index += 1) {
    if (wsCheck(linePrefix[index] as string)) {
      linePrefixHasWhitespace = true;
      break;
    }
  }

  if (linePrefix.length > 0 && !linePrefixHasWhitespace) {
    const triggerChar = linePrefix[0];
    if (triggerChar === ":") {
      return {
        kind: "coddy-studio-action",
        query: linePrefix.slice(1),
        rangeStart: lineStart,
        rangeEnd: cursor,
      };
    }
    if (triggerChar === "/" && profile.providerSlash) {
      return {
        kind: "provider-slash",
        query: linePrefix.slice(1),
        rangeStart: lineStart,
        rangeEnd: cursor,
      };
    }
  }

  let tokenIdx = cursor - 1;
  while (tokenIdx >= 0 && !wsCheck(text[tokenIdx] as string)) {
    tokenIdx -= 1;
  }
  const tokenStart = tokenIdx + 1;

  const token = text.slice(tokenStart, cursor);
  if (token.startsWith("$") && profile.providerDollarSkill) {
    return {
      kind: "provider-dollar-skill",
      query: token.slice(1),
      rangeStart: tokenStart,
      rangeEnd: cursor,
    };
  }
  if (!token.startsWith("@")) {
    return null;
  }

  return {
    kind: "provider-reference",
    query: token.slice(1),
    rangeStart: tokenStart,
    rangeEnd: cursor,
  };
}

export function parseStandaloneComposerCoddyStudioAction(text: string): ComposerCoddyStudioAction | null {
  const match = /^:(model|plan|default)\s*$/i.exec(text.trim());
  if (!match) {
    return null;
  }
  const command = match[1]?.toLowerCase();
  if (command === "model") return "model";
  if (command === "plan") return "plan";
  return "default";
}

export function replaceTextRange(
  text: string,
  rangeStart: number,
  rangeEnd: number,
  replacement: string,
): { text: string; cursor: number } {
  const safeStart = Math.max(0, Math.min(text.length, rangeStart));
  const safeEnd = Math.max(safeStart, Math.min(text.length, rangeEnd));
  const nextText = `${text.slice(0, safeStart)}${replacement}${text.slice(safeEnd)}`;
  return { text: nextText, cursor: safeStart + replacement.length };
}
