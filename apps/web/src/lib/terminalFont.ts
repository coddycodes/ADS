import { TerminalCustomFontFamily, type TerminalFontPreference } from "@coddy/contracts/settings";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

export const TERMINAL_NERD_FONT_FAMILY = "Coddy Studio JetBrainsMono Nerd Font Mono";
export const TERMINAL_FONT_GLYPH_PROBE = "\ue0b0\uf115\u{f0001}";

interface TerminalFontFaceSet {
  readonly check: (font: string) => boolean;
}

interface TerminalFontLoader {
  readonly load: (font: string, text?: string) => Promise<unknown>;
}

type TerminalFontWarning = (message: string, cause: unknown) => void;

const decodeCustomFontFamily = Schema.decodeUnknownOption(TerminalCustomFontFamily);
let bundledFontLoadPromise: Promise<void> | null = null;

export function normalizeCustomTerminalFontFamily(input: string): string | null {
  return Option.getOrNull(decodeCustomFontFamily(input));
}

function quoteFontFamily(family: string): string {
  return `"${family.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;
}

export function resolveTerminalFontFamily(preference: TerminalFontPreference): string {
  switch (preference.mode) {
    case "system":
      return "ui-monospace, monospace";
    case "custom":
      return `${quoteFontFamily(preference.family)}, monospace`;
    case "bundled":
      return `${quoteFontFamily(TERMINAL_NERD_FONT_FAMILY)}, monospace`;
  }
}

export function isCustomTerminalFontAvailable(
  family: string,
  fontSet: TerminalFontFaceSet | null = typeof document === "undefined" ? null : document.fonts,
): boolean | null {
  if (fontSet === null) return null;
  try {
    return fontSet.check(`12px ${quoteFontFamily(family)}`);
  } catch {
    return false;
  }
}

export function ensureBundledTerminalFontLoaded(
  fontSet: TerminalFontLoader = document.fonts,
  warn: TerminalFontWarning = (message, cause) => console.warn(message, { cause }),
): Promise<void> {
  if (bundledFontLoadPromise === null) {
    bundledFontLoadPromise = fontSet
      .load(`12px ${quoteFontFamily(TERMINAL_NERD_FONT_FAMILY)}`, TERMINAL_FONT_GLYPH_PROBE)
      .then(() => undefined)
      .catch((cause: unknown) => {
        warn("[terminal] Bundled Nerd Font failed to load; using text fallbacks.", cause);
      });
  }
  return bundledFontLoadPromise;
}

export function __resetTerminalFontLoaderForTests(): void {
  bundledFontLoadPromise = null;
}
