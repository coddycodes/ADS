import { describe, expect, it } from "vite-plus/test";
import {
  mergeTerminalSpawnEnv,
  resolveTerminalThemeMode,
  terminalExtendedAnsiPalette,
  retainTerminalLaunchTheme,
  terminalOscColorEnv,
  usesPersistentWindowsConsoleTheme,
} from "./terminalTheme";

describe("terminal theme launch values", () => {
  it("maps resolved light and dark themes to exact OSC colors", () => {
    expect(terminalOscColorEnv("light")).toEqual({
      CODDY_STUDIO_OSC_BACKGROUND: "255,255,255",
      CODDY_STUDIO_OSC_FOREGROUND: "28,33,41",
      CODDY_STUDIO_OSC_CURSOR: "38,56,78",
    });
    expect(terminalOscColorEnv("dark")).toEqual({
      CODDY_STUDIO_OSC_BACKGROUND: "14,18,24",
      CODDY_STUDIO_OSC_FOREGROUND: "237,241,247",
      CODDY_STUDIO_OSC_CURSOR: "180,203,255",
    });
  });

  it("replaces exact reserved keys with authoritative values inserted last", () => {
    const merged = mergeTerminalSpawnEnv({
      commandEnv: {
        SHARED: "command",
        COMMAND_ONLY: "yes",
        CODDY_STUDIO_OSC_BACKGROUND: "0,0,0",
        CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "command",
      },
      runtimeEnv: {
        SHARED: "runtime",
        RUNTIME_ONLY: "yes",
        CODDY_STUDIO_OSC_FOREGROUND: "0,0,0",
        CODDY_STUDIO_OSC_CURSOR: "0,0,0",
        CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "runtime",
      },
      resolvedTheme: "light",
      windowsConsoleTheme: true,
    });

    expect(merged).toEqual({
      SHARED: "runtime",
      COMMAND_ONLY: "yes",
      RUNTIME_ONLY: "yes",
      CODDY_STUDIO_OSC_BACKGROUND: "255,255,255",
      CODDY_STUDIO_OSC_FOREGROUND: "28,33,41",
      CODDY_STUDIO_OSC_CURSOR: "38,56,78",
      CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "light",
    });
    expect(Object.keys(merged).slice(-4)).toEqual([
      "CODDY_STUDIO_OSC_BACKGROUND",
      "CODDY_STUDIO_OSC_FOREGROUND",
      "CODDY_STUDIO_OSC_CURSOR",
      "CODDY_STUDIO_WINDOWS_CONSOLE_THEME",
    ]);
  });

  it("preserves differently cased variables for case-sensitive platforms", () => {
    expect(
      mergeTerminalSpawnEnv({
        commandEnv: {
          coddy_studio_osc_foreground: "command-lowercase",
          CoddyStudio_Osc_Cursor: "command-mixed-case",
          coddy_studio_windows_console_theme: "command-lowercase",
        },
        runtimeEnv: {
          coddy_studio_osc_background: "runtime-lowercase",
          CoddyStudio_Windows_Console_Theme: "runtime-mixed-case",
        },
        resolvedTheme: "light",
        windowsConsoleTheme: true,
      }),
    ).toEqual({
      coddy_studio_osc_foreground: "command-lowercase",
      CoddyStudio_Osc_Cursor: "command-mixed-case",
      coddy_studio_windows_console_theme: "command-lowercase",
      coddy_studio_osc_background: "runtime-lowercase",
      CoddyStudio_Windows_Console_Theme: "runtime-mixed-case",
      CODDY_STUDIO_OSC_BACKGROUND: "255,255,255",
      CODDY_STUDIO_OSC_FOREGROUND: "28,33,41",
      CODDY_STUDIO_OSC_CURSOR: "38,56,78",
      CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "light",
    });
  });

  it("omits the Windows console theme when it is not requested", () => {
    expect(
      mergeTerminalSpawnEnv({
        commandEnv: { CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "command" },
        runtimeEnv: { CODDY_STUDIO_WINDOWS_CONSOLE_THEME: "runtime" },
        resolvedTheme: "dark",
        windowsConsoleTheme: false,
      }),
    ).toEqual({
      CODDY_STUDIO_OSC_BACKGROUND: "14,18,24",
      CODDY_STUDIO_OSC_FOREGROUND: "237,241,247",
      CODDY_STUDIO_OSC_CURSOR: "180,203,255",
    });
  });

  it("retains a Codex launch theme until the terminal generation changes", () => {
    const initial = retainTerminalLaunchTheme(null, {
      persistentConsoleTheme: true,
      generation: 4,
      resolvedTheme: "dark",
    });

    expect(
      retainTerminalLaunchTheme(initial, {
        persistentConsoleTheme: true,
        generation: 4,
        resolvedTheme: "light",
      }),
    ).toEqual({ generation: 4, theme: "dark" });
    expect(
      retainTerminalLaunchTheme(initial, {
        persistentConsoleTheme: true,
        generation: 5,
        resolvedTheme: "light",
      }),
    ).toEqual({ generation: 5, theme: "light" });
  });

  it("uses the requested process theme when that restart advances the generation", () => {
    const previous = { generation: 4, theme: "dark" } as const;

    expect(
      retainTerminalLaunchTheme(previous, {
        persistentConsoleTheme: true,
        generation: 5,
        resolvedTheme: "dark",
        restartRequest: { sourceGeneration: 4, targetTheme: "light" },
      }),
    ).toEqual({ generation: 5, theme: "light" });
  });

  it("tracks the resolved theme live for non-Codex terminals", () => {
    const initial = retainTerminalLaunchTheme(null, {
      persistentConsoleTheme: false,
      generation: 4,
      resolvedTheme: "dark",
    });

    expect(
      retainTerminalLaunchTheme(initial, {
        persistentConsoleTheme: false,
        generation: 4,
        resolvedTheme: "light",
      }),
    ).toEqual({ generation: 4, theme: "light" });
  });
  it("keeps the terminal on its own theme unless it is told to follow the app", () => {
    // Codex paints hardcoded dark panels and never queries the terminal, so a
    // dark terminal inside a light app is the correct default, not a mismatch.
    expect(resolveTerminalThemeMode("dark", "light")).toBe("dark");
    expect(resolveTerminalThemeMode("dark", "dark")).toBe("dark");
    expect(resolveTerminalThemeMode("light", "dark")).toBe("light");
    expect(resolveTerminalThemeMode("light", "light")).toBe("light");
  });

  it("tracks the app theme only for the explicit follow preference", () => {
    expect(resolveTerminalThemeMode("app", "light")).toBe("light");
    expect(resolveTerminalThemeMode("app", "dark")).toBe("dark");
  });
  it("flips only the grayscale ramp so hardcoded TUI surfaces follow a light terminal", () => {
    const dark = terminalExtendedAnsiPalette("dark");
    const light = terminalExtendedAnsiPalette("light");

    // Indices 16..255 inclusive.
    expect(dark).toHaveLength(240);
    expect(light).toHaveLength(240);

    // Codex fills its panels with index 235, deep in the grayscale ramp.
    const at = (palette: ReadonlyArray<string>, index: number) => palette[index - 16];
    expect(at(dark, 235)).toBe("#262626");
    expect(at(light, 235)).toBe("#d0d0d0");

    // The ramp ends swap, and the 6x6x6 colour cube is untouched in both.
    expect(at(dark, 232)).toBe(at(light, 255));
    expect(at(dark, 255)).toBe(at(light, 232));
    expect(at(dark, 196)).toBe("#ff0000");
    expect(at(light, 196)).toBe("#ff0000");
    expect(dark.slice(0, 216)).toEqual(light.slice(0, 216));
  });

  it("detects Codex launches that pin the spawn theme until restart", () => {
    expect(usesPersistentWindowsConsoleTheme({ executable: "codex", args: [] })).toBe(true);
    expect(
      usesPersistentWindowsConsoleTheme({
        executable: "/usr/local/bin/codex",
        args: ["--dangerously-bypass-approvals-and-sandbox"],
      }),
    ).toBe(true);
    expect(usesPersistentWindowsConsoleTheme({ executable: "claude", args: [] })).toBe(false);
    expect(usesPersistentWindowsConsoleTheme(undefined)).toBe(false);
  });
});
