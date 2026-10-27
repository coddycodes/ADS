import type { TerminalThemePreference } from "@coddy/contracts/settings";

export type TerminalThemeMode = "light" | "dark";

export interface TerminalLaunchThemeState {
  readonly generation: number;
  readonly theme: TerminalThemeMode;
}

const TERMINAL_OSC_COLORS = {
  dark: { background: "14,18,24", foreground: "237,241,247", cursor: "180,203,255" },
  light: { background: "255,255,255", foreground: "28,33,41", cursor: "38,56,78" },
} as const;

const WINDOWS_CONSOLE_THEME = "CODDY_STUDIO_WINDOWS_CONSOLE_THEME";
const RESERVED_TERMINAL_THEME_ENV = new Set([
  "CODDY_STUDIO_OSC_BACKGROUND",
  "CODDY_STUDIO_OSC_FOREGROUND",
  "CODDY_STUDIO_OSC_CURSOR",
  WINDOWS_CONSOLE_THEME,
]);

function withoutReservedTerminalThemeEnv(
  env: Readonly<Record<string, string>> | undefined,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(env ?? {}).filter(([key]) => !RESERVED_TERMINAL_THEME_ENV.has(key)),
  );
}

/**
 * Resolves the theme the terminal surface renders with.
 *
 * The terminal is deliberately decoupled from the app theme. Provider TUIs such
 * as Codex paint their own surfaces with hardcoded dark 256-colour indices and
 * never query the terminal for its colours, so forcing a light terminal renders
 * their panels as dark blocks. `app` is the opt-in for users who want the
 * terminal to track the app instead.
 */
export function resolveTerminalThemeMode(
  preference: TerminalThemePreference,
  appTheme: TerminalThemeMode,
): TerminalThemeMode {
  return preference === "app" ? appTheme : preference;
}

const XTERM_CUBE_LEVELS = [0, 95, 135, 175, 215, 255] as const;
const GRAYSCALE_RAMP_START = 232;
const GRAYSCALE_RAMP_STEPS = 24;

function rgbHex(red: number, green: number, blue: number): string {
  const channel = (value: number) => value.toString(16).padStart(2, "0");
  return `#${channel(red)}${channel(green)}${channel(blue)}`;
}

/**
 * The 240 colours xterm indexes from 16 to 255, with the grayscale ramp flipped
 * for a light terminal.
 *
 * Full-screen TUIs paint their own surfaces from the grayscale ramp with
 * hardcoded indices — Codex fills its panels with 235 (#262626) — and never ask
 * the terminal which colours it uses. Serving the standard ramp to a light
 * terminal therefore renders those panels as dark blocks. Flipping only the
 * ramp turns a TUI's "darkest surface" into the lightest one, so the same
 * hardcoded index lands on a surface that suits the terminal. The 6x6x6 colour
 * cube is left untouched: those are chosen hues, not surface shades, and
 * altering them would misrepresent real content.
 */
export function terminalExtendedAnsiPalette(mode: TerminalThemeMode): Array<string> {
  const palette: Array<string> = [];
  for (let index = 16; index < GRAYSCALE_RAMP_START; index += 1) {
    const offset = index - 16;
    palette.push(
      rgbHex(
        XTERM_CUBE_LEVELS[Math.floor(offset / 36) % 6]!,
        XTERM_CUBE_LEVELS[Math.floor(offset / 6) % 6]!,
        XTERM_CUBE_LEVELS[offset % 6]!,
      ),
    );
  }
  for (let step = 0; step < GRAYSCALE_RAMP_STEPS; step += 1) {
    const position = mode === "light" ? GRAYSCALE_RAMP_STEPS - 1 - step : step;
    const level = 8 + position * 10;
    palette.push(rgbHex(level, level, level));
  }
  return palette;
}

export function terminalOscColorEnv(mode: TerminalThemeMode): Record<string, string> {
  const colors = TERMINAL_OSC_COLORS[mode];
  return {
    CODDY_STUDIO_OSC_BACKGROUND: colors.background,
    CODDY_STUDIO_OSC_FOREGROUND: colors.foreground,
    CODDY_STUDIO_OSC_CURSOR: colors.cursor,
  };
}

export function mergeTerminalSpawnEnv(input: {
  readonly commandEnv?: Readonly<Record<string, string>> | undefined;
  readonly runtimeEnv?: Readonly<Record<string, string>> | undefined;
  readonly resolvedTheme: TerminalThemeMode;
  readonly windowsConsoleTheme: boolean;
}): Record<string, string> {
  return {
    ...withoutReservedTerminalThemeEnv(input.commandEnv),
    ...withoutReservedTerminalThemeEnv(input.runtimeEnv),
    ...terminalOscColorEnv(input.resolvedTheme),
    ...(input.windowsConsoleTheme ? { [WINDOWS_CONSOLE_THEME]: input.resolvedTheme } : {}),
  };
}

/** Codex snapshots OSC (and on Windows, ConPTY) colors at spawn; live xterm theme changes leave unreadable UI. */
export function usesPersistentWindowsConsoleTheme(
  command:
    | {
        readonly executable: string;
        readonly args: ReadonlyArray<string>;
      }
    | null
    | undefined,
): boolean {
  if (!command) return false;
  const executable = command.executable.split(/[\\/]/).at(-1)?.toLowerCase() ?? "";
  return (
    executable.includes("codex") ||
    command.args.includes("--dangerously-bypass-approvals-and-sandbox")
  );
}

export function retainTerminalLaunchTheme(
  previous: TerminalLaunchThemeState | null,
  input: {
    readonly persistentConsoleTheme: boolean;
    readonly generation: number;
    readonly resolvedTheme: TerminalThemeMode;
    readonly authoritativeTheme?: TerminalThemeMode | null;
    readonly restartRequest?: {
      readonly sourceGeneration: number;
      readonly targetTheme: TerminalThemeMode;
    } | null;
  },
): TerminalLaunchThemeState {
  if (
    input.persistentConsoleTheme &&
    input.authoritativeTheme !== null &&
    input.authoritativeTheme !== undefined
  ) {
    return { generation: input.generation, theme: input.authoritativeTheme };
  }
  if (!input.persistentConsoleTheme || previous === null) {
    return { generation: input.generation, theme: input.resolvedTheme };
  }
  if (previous.generation !== input.generation) {
    const requestedTheme =
      input.restartRequest !== null &&
      input.restartRequest !== undefined &&
      input.generation > input.restartRequest.sourceGeneration
        ? input.restartRequest.targetTheme
        : input.resolvedTheme;
    return { generation: input.generation, theme: requestedTheme };
  }
  return previous;
}
