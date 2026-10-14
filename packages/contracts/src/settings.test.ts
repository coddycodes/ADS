import { describe, expect, it } from "vite-plus/test";
import * as Duration from "effect/Duration";
import * as Schema from "effect/Schema";

import { ProviderDriverKind, ProviderInstanceId } from "./providerInstance.ts";
import {
  BUNDLED_TERMINAL_FONT_PREFERENCE,
  ClientSettingsPatch,
  ClientSettingsSchema,
  CodexSettings,
  CursorSettings,
  DEFAULT_CLIENT_SETTINGS,
  DEFAULT_SERVER_SETTINGS,
  DEFAULT_UNIFIED_SETTINGS,
  ProviderSessionDefault,
  ProviderSessionDefaultsMap,
  ServerSettings,
  ServerSettingsError,
  ServerSettingsPatch,
  WorktreeWorkspaceError,
  makeProviderSettingsSchema,
} from "./settings.ts";

const decodeClientSettings = Schema.decodeUnknownSync(ClientSettingsSchema);
const decodeClientSettingsPatch = Schema.decodeUnknownSync(ClientSettingsPatch);
const decodeServerSettings = Schema.decodeUnknownSync(ServerSettings);
const decodeServerSettingsPatch = Schema.decodeUnknownSync(ServerSettingsPatch);
const decodeWorktreeWorkspaceError = Schema.decodeUnknownSync(WorktreeWorkspaceError);
const encodeServerSettings = Schema.encodeSync(ServerSettings);
const decodeProviderSessionDefault = Schema.decodeUnknownSync(ProviderSessionDefault);
const decodeProviderSessionDefaultsMap = Schema.decodeUnknownSync(ProviderSessionDefaultsMap);
const encodeProviderSessionDefault = Schema.encodeSync(ProviderSessionDefault);
const encodeProviderSessionDefaultsMap = Schema.encodeSync(ProviderSessionDefaultsMap);
const decodeCodexSettings = Schema.decodeUnknownSync(CodexSettings);
const decodeCursorSettings = Schema.decodeUnknownSync(CursorSettings);

describe("ClientSettings Pull Requests", () => {
  it("enables the module by default and preserves an explicit opt-out", () => {
    expect(decodeClientSettings({}).pullRequestsEnabled).toBe(true);
    expect(DEFAULT_CLIENT_SETTINGS.pullRequestsEnabled).toBe(true);
    expect(decodeClientSettings({ pullRequestsEnabled: false }).pullRequestsEnabled).toBe(false);
  });

  it("patches the preference without resetting an omitted value", () => {
    expect(decodeClientSettingsPatch({ pullRequestsEnabled: false })).toMatchObject({
      pullRequestsEnabled: false,
    });
    expect(decodeClientSettingsPatch({})).not.toHaveProperty("pullRequestsEnabled");
    expect(() => decodeClientSettingsPatch({ pullRequestsEnabled: "false" })).toThrow();
  });
});

describe("ClientSettings repository sort order", () => {
  it("sorts repositories by name by default and keeps an explicit choice", () => {
    expect(decodeClientSettings({}).sidebarRepositorySortOrder).toBe("name");
    expect(DEFAULT_CLIENT_SETTINGS.sidebarRepositorySortOrder).toBe("name");
    expect(
      decodeClientSettings({ sidebarRepositorySortOrder: "updated_at" }).sidebarRepositorySortOrder,
    ).toBe("updated_at");
    expect(decodeClientSettingsPatch({ sidebarRepositorySortOrder: "created_at" })).toMatchObject({
      sidebarRepositorySortOrder: "created_at",
    });
    expect(() => decodeClientSettingsPatch({ sidebarRepositorySortOrder: "manual" })).toThrow();
  });
});

describe("ServerSettings Grok defaults", () => {
  it("defaults Grok to disabled for legacy and fresh settings", () => {
    expect(decodeServerSettings({}).providers.grok.enabled).toBe(false);
    expect(DEFAULT_SERVER_SETTINGS.providers.grok.enabled).toBe(false);
  });
});

describe("ServerSettings supported provider defaults", () => {
  it("enables Cursor for fresh settings while preserving an explicit disable", () => {
    expect(decodeServerSettings({}).providers.cursor).toMatchObject({
      enabled: true,
      binaryPath: "cursor-agent",
    });
    expect(DEFAULT_SERVER_SETTINGS.providers.cursor).toMatchObject({
      enabled: true,
      binaryPath: "cursor-agent",
    });
    expect(
      decodeServerSettings({ providers: { cursor: { enabled: false } } }).providers.cursor.enabled,
    ).toBe(false);
  });
});

describe("ServerSettings agent activity", () => {
  it("defaults Chat on and AI Terminal off", () => {
    expect(decodeServerSettings({})).toMatchObject({
      enableChatAgentActivity: true,
      enableTerminalAgentActivity: false,
    });
    expect(DEFAULT_SERVER_SETTINGS).toMatchObject({
      enableChatAgentActivity: true,
      enableTerminalAgentActivity: false,
    });
  });

  it("decodes and patches both activity settings independently", () => {
    expect(
      decodeServerSettings({
        enableChatAgentActivity: false,
        enableTerminalAgentActivity: true,
      }),
    ).toMatchObject({
      enableChatAgentActivity: false,
      enableTerminalAgentActivity: true,
    });
    expect(decodeServerSettingsPatch({ enableChatAgentActivity: false })).toEqual({
      enableChatAgentActivity: false,
    });
    expect(decodeServerSettingsPatch({ enableTerminalAgentActivity: true })).toEqual({
      enableTerminalAgentActivity: true,
    });
  });

  it("rejects non-boolean activity settings", () => {
    expect(() => decodeServerSettings({ enableChatAgentActivity: "false" })).toThrow();
    expect(() => decodeServerSettings({ enableTerminalAgentActivity: 1 })).toThrow();
    expect(() => decodeServerSettingsPatch({ enableChatAgentActivity: 0 })).toThrow();
    expect(() => decodeServerSettingsPatch({ enableTerminalAgentActivity: "true" })).toThrow();
  });
});

describe("ServerSettings device previews", () => {
  it("defaults enableDevicePreviews to false and accepts a patch", () => {
    expect(DEFAULT_SERVER_SETTINGS.enableDevicePreviews).toBe(false);
    expect(decodeServerSettingsPatch({ enableDevicePreviews: true })).toEqual({
      enableDevicePreviews: true,
    });
  });

  it("rejects a non-boolean device previews toggle", () => {
    expect(() => decodeServerSettingsPatch({ enableDevicePreviews: "yes" })).toThrow();
  });
});

describe("ServerSettings profiler", () => {
  it("defaults off and patches the toggle", () => {
    expect(decodeServerSettings({}).enableProfiler).toBe(false);
    expect(decodeServerSettings({ enableProfiler: true }).enableProfiler).toBe(true);
    expect(decodeServerSettingsPatch({ enableProfiler: true })).toEqual({ enableProfiler: true });
  });

  it("rejects a non-boolean profiler toggle", () => {
    expect(() => decodeServerSettings({ enableProfiler: "true" })).toThrow();
    expect(() => decodeServerSettingsPatch({ enableProfiler: 1 })).toThrow();
  });
});

describe("ServerSettings automatic Git fetch", () => {
  it("defaults fresh and legacy settings to three minutes", () => {
    expect(Duration.toMillis(decodeServerSettings({}).automaticGitFetchInterval)).toBe(180_000);
    expect(Duration.toMillis(DEFAULT_SERVER_SETTINGS.automaticGitFetchInterval)).toBe(180_000);
    expect(Duration.toMillis(DEFAULT_UNIFIED_SETTINGS.automaticGitFetchInterval)).toBe(180_000);
  });
});

describe("ServerSettings default agent", () => {
  it("defaults to Codex chat", () => {
    expect(decodeServerSettings({}).defaultAgent).toEqual({
      kind: "chat",
      instanceId: "codex",
    });
    expect(DEFAULT_SERVER_SETTINGS.defaultAgent).toEqual({
      kind: "chat",
      instanceId: "codex",
    });
  });

  it.each([
    { kind: "chat", instanceId: "claudeAgent" },
    { kind: "terminal", instanceId: "codex_work" },
  ])("decodes and patches $kind selections", (defaultAgent) => {
    expect(decodeServerSettings({ defaultAgent }).defaultAgent).toEqual(defaultAgent);
    expect(decodeServerSettingsPatch({ defaultAgent }).defaultAgent).toEqual(defaultAgent);
  });

  it.each([
    null,
    {},
    { kind: "shell", instanceId: "codex" },
    { kind: "chat", instanceId: "" },
    { kind: "terminal" },
  ])("rejects malformed default agents", (defaultAgent) => {
    expect(() => decodeServerSettings({ defaultAgent })).toThrow();
    expect(() => decodeServerSettingsPatch({ defaultAgent })).toThrow();
  });
});

describe("ServerSettings terminal", () => {
  it("defaults webglEnabled to true for legacy configs", () => {
    expect(decodeServerSettings({}).terminal.webglEnabled).toBe(true);
  });

  it("decodes an explicit false", () => {
    expect(decodeServerSettings({ terminal: { webglEnabled: false } }).terminal.webglEnabled).toBe(
      false,
    );
  });

  it("decodes an explicit true", () => {
    expect(decodeServerSettings({ terminal: { webglEnabled: true } }).terminal.webglEnabled).toBe(
      true,
    );
  });

  it("exposes terminal defaults from server and unified settings", () => {
    expect(DEFAULT_SERVER_SETTINGS.terminal.webglEnabled).toBe(true);
    expect(DEFAULT_UNIFIED_SETTINGS.terminal.webglEnabled).toBe(true);
  });

  it("decodes a webglEnabled patch", () => {
    expect(
      decodeServerSettingsPatch({ terminal: { webglEnabled: false } }).terminal?.webglEnabled,
    ).toBe(false);
  });

  it("accepts an empty nested terminal patch", () => {
    expect(decodeServerSettingsPatch({ terminal: {} }).terminal).toEqual({});
  });

  it("treats an omitted terminal patch as undefined", () => {
    expect(decodeServerSettingsPatch({}).terminal).toBeUndefined();
  });

  it("rejects non-boolean webglEnabled values", () => {
    expect(() => decodeServerSettings({ terminal: { webglEnabled: "false" } })).toThrow();
    expect(() => decodeServerSettingsPatch({ terminal: { webglEnabled: 1 } })).toThrow();
  });
});

describe("ClientSettings terminal font", () => {
  it("automatically defaults legacy settings to the bundled font", () => {
    expect(decodeClientSettings({}).terminalFontPreference).toEqual(
      BUNDLED_TERMINAL_FONT_PREFERENCE,
    );
    expect(DEFAULT_CLIENT_SETTINGS.terminalFontPreference).toEqual(
      BUNDLED_TERMINAL_FONT_PREFERENCE,
    );
  });

  it("decodes each supported preference", () => {
    expect(
      decodeClientSettings({ terminalFontPreference: { mode: "system" } }).terminalFontPreference,
    ).toEqual({ mode: "system" });
    expect(
      decodeClientSettings({
        terminalFontPreference: {
          mode: "custom",
          family: "  Iosevka Nerd Font  ",
        },
      }).terminalFontPreference,
    ).toEqual({ mode: "custom", family: "Iosevka Nerd Font" });
  });

  it.each([
    { mode: "obsolete" },
    { mode: "custom", family: "" },
    { mode: "custom", family: "Font, monospace" },
    { mode: "custom", family: "Font\u0000Name" },
  ])("recovers malformed preferences to bundled: %o", (terminalFontPreference) => {
    const decoded = decodeClientSettings({
      wordWrap: false,
      terminalFontPreference,
    });

    expect(decoded.wordWrap).toBe(false);
    expect(decoded.terminalFontPreference).toEqual(BUNDLED_TERMINAL_FONT_PREFERENCE);
  });

  it("accepts a device-local terminal font patch", () => {
    expect(
      decodeClientSettingsPatch({
        terminalFontPreference: { mode: "custom", family: "Maple Mono" },
      }).terminalFontPreference,
    ).toEqual({ mode: "custom", family: "Maple Mono" });
  });
});

describe("ClientSettings terminal theme", () => {
  it("follows the app theme for fresh, legacy, and malformed device settings", () => {
    expect(decodeClientSettings({}).terminalThemePreference).toBe("app");
    expect(
      decodeClientSettings({ terminalThemePreference: "obsolete" }).terminalThemePreference,
    ).toBe("app");
    expect(DEFAULT_CLIENT_SETTINGS.terminalThemePreference).toBe("app");
    expect(DEFAULT_UNIFIED_SETTINGS.terminalThemePreference).toBe("app");
  });

  it("preserves explicit dark and light terminal theme choices", () => {
    expect(decodeClientSettings({ terminalThemePreference: "dark" }).terminalThemePreference).toBe(
      "dark",
    );
    expect(decodeClientSettings({ terminalThemePreference: "light" }).terminalThemePreference).toBe(
      "light",
    );
  });
});

describe("ClientSettings word wrap", () => {
  it("defaults word wrap on", () => {
    expect(decodeClientSettings({}).wordWrap).toBe(true);
  });

  it("ignores obsolete wrapping preferences", () => {
    const decoded = decodeClientSettings({
      chatWordWrap: false,
      diffWordWrap: false,
    });

    expect(decoded.wordWrap).toBe(true);
    expect(decoded).not.toHaveProperty("chatWordWrap");
    expect(decoded).not.toHaveProperty("diffWordWrap");
  });
});

describe("ClientSettings usage status bar", () => {
  it("defaults the usage display and status bar items", () => {
    const settings = decodeClientSettings({});

    expect({
      usagePercentageDisplay: settings.usagePercentageDisplay,
      statusBarUsageMode: settings.statusBarUsageMode,
      statusBarItems: settings.statusBarItems,
    }).toEqual({
      usagePercentageDisplay: "remaining",
      statusBarUsageMode: "detailed",
      statusBarItems: ["claude", "codex", "resource-usage"],
    });
  });

  it("decodes valid usage status bar overrides", () => {
    expect(
      decodeClientSettings({
        usagePercentageDisplay: "used",
        statusBarUsageMode: "compact",
        statusBarItems: ["resource-usage", "codex"],
      }),
    ).toMatchObject({
      usagePercentageDisplay: "used",
      statusBarUsageMode: "compact",
      statusBarItems: ["resource-usage", "codex"],
    });
  });

  it("individually recovers invalid usage status bar fields", () => {
    expect(
      decodeClientSettings({
        wordWrap: false,
        usagePercentageDisplay: "invalid",
        statusBarUsageMode: "invalid",
        statusBarItems: ["invalid"],
      }),
    ).toMatchObject({
      wordWrap: false,
      usagePercentageDisplay: "remaining",
      statusBarUsageMode: "detailed",
      statusBarItems: ["claude", "codex", "resource-usage"],
    });
  });

  it("preserves valid usage status bar fields beside an invalid field", () => {
    expect(
      decodeClientSettings({
        usagePercentageDisplay: "invalid",
        statusBarUsageMode: "compact",
        statusBarItems: ["codex"],
      }),
    ).toMatchObject({
      usagePercentageDisplay: "remaining",
      statusBarUsageMode: "compact",
      statusBarItems: ["codex"],
    });
  });
});

describe("ServerSettings.providerSessionDefaults", () => {
  it("defaults legacy settings and the shared default settings object to an empty map", () => {
    expect(decodeServerSettings({}).providerSessionDefaults).toEqual({});
    expect(DEFAULT_SERVER_SETTINGS.providerSessionDefaults).toEqual({});
  });

  it("decodes defaults for both built-in and open driver slugs", () => {
    const defaults = decodeProviderSessionDefaultsMap({
      codex: { model: "gpt-5.4" },
      ollama_local: { model: "llama3.3" },
    });

    expect(defaults).toEqual({
      codex: { model: "gpt-5.4" },
      ollama_local: { model: "llama3.3" },
    });
  });

  it("round-trips canonical option selections", () => {
    const decoded = decodeProviderSessionDefault({
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });

    expect(encodeProviderSessionDefault(decoded)).toEqual({
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });
  });

  it("normalizes legacy object-shaped option selections to the canonical array", () => {
    expect(
      decodeProviderSessionDefault({
        model: "gpt-5.4",
        options: { reasoningEffort: "high", fastMode: true },
      }),
    ).toEqual({
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });
  });

  it("trims provider-session defaults while encoding", () => {
    expect(
      encodeProviderSessionDefaultsMap({
        [ProviderDriverKind.make("codex")]: {
          model: "  gpt-5.4  ",
          options: [{ id: "  reasoningEffort  ", value: "  high  " }],
        },
      }),
    ).toEqual({
      codex: {
        model: "gpt-5.4",
        options: [{ id: "reasoningEffort", value: "high" }],
      },
    });
  });
});

describe("ServerSettingsPatch.providerSessionDefaults", () => {
  it("leaves an omitted default map undefined", () => {
    expect(decodeServerSettingsPatch({}).providerSessionDefaults).toBeUndefined();
  });

  it("decodes a supplied default map as a whole replacement", () => {
    expect(
      decodeServerSettingsPatch({
        providerSessionDefaults: {
          codex: { model: "gpt-5.4" },
          ollama_local: { model: "llama3.3" },
        },
      }).providerSessionDefaults,
    ).toEqual({
      codex: { model: "gpt-5.4" },
      ollama_local: { model: "llama3.3" },
    });
  });
});

describe("ServerSettings.providerInstances (slice-2 invariant)", () => {
  it("defaults to an empty record so legacy configs without the key still decode", () => {
    expect(DEFAULT_SERVER_SETTINGS.providerInstances).toEqual({});
  });

  it("decodes a fully empty config (legacy on-disk shape) without complaint", () => {
    const decoded = decodeServerSettings({});
    expect(decoded.providerInstances).toEqual({});
    // Legacy `providers` struct is still hydrated with its per-driver defaults
    // so existing call sites keep working through the migration.
    expect(decoded.providers.codex.enabled).toBe(true);
  });

  it("decodes a multi-instance map mixing first-party and fork drivers", () => {
    const decoded = decodeServerSettings({
      providerInstances: {
        codex_personal: {
          driver: "codex",
          displayName: "Codex (personal)",
          config: { homePath: "~/.codex_personal" },
        },
        codex_work: {
          driver: "codex",
          config: { homePath: "~/.codex_work" },
        },
        ollama_local: {
          driver: "ollama",
          displayName: "Ollama (local)",
          config: { endpoint: "http://localhost:11434" },
        },
      },
    });
    const personalId = ProviderInstanceId.make("codex_personal");
    const workId = ProviderInstanceId.make("codex_work");
    const ollamaId = ProviderInstanceId.make("ollama_local");

    expect(decoded.providerInstances[personalId]?.driver).toBe("codex");
    expect(decoded.providerInstances[workId]?.config).toEqual({ homePath: "~/.codex_work" });
    // Critical: a config naming a driver this build does not know about
    // (`ollama` is not in `ProviderDriverKind`) must round-trip without loss.
    // The runtime handles "driver not installed" — the schema must not.
    expect(decoded.providerInstances[ollamaId]?.driver).toBe("ollama");
    expect(decoded.providerInstances[ollamaId]?.config).toEqual({
      endpoint: "http://localhost:11434",
    });
  });

  it("rejects instance keys that violate the slug pattern", () => {
    expect(() =>
      decodeServerSettings({
        providerInstances: { "1bad": { driver: "codex" } },
      }),
    ).toThrow();
  });
});

describe("ServerSettings worktree defaults", () => {
  it("defaults start-from-origin off for legacy configs", () => {
    expect(decodeServerSettings({}).newWorktreesStartFromOrigin).toBe(false);
  });

  it("accepts start-from-origin updates", () => {
    expect(
      decodeServerSettingsPatch({ newWorktreesStartFromOrigin: true }).newWorktreesStartFromOrigin,
    ).toBe(true);
  });
});

describe("ServerSettings worktree workspace", () => {
  it("defaults legacy documents to the project-adjacent workspace", () => {
    expect(decodeServerSettings({}).worktreeBaseDirectory).toBe("");
    expect(DEFAULT_SERVER_SETTINGS.worktreeBaseDirectory).toBe("");
  });

  it("trims configured workspace settings and patches", () => {
    expect(
      decodeServerSettings({ worktreeBaseDirectory: "  ~/Worktrees  " }).worktreeBaseDirectory,
    ).toBe("~/Worktrees");
    expect(
      decodeServerSettingsPatch({ worktreeBaseDirectory: "  D:\\Worktrees  " })
        .worktreeBaseDirectory,
    ).toBe("D:\\Worktrees");
  });

  it("rejects non-string workspace settings", () => {
    expect(() => decodeServerSettings({ worktreeBaseDirectory: 42 })).toThrow();
    expect(() => decodeServerSettingsPatch({ worktreeBaseDirectory: false })).toThrow();
  });
});

describe("ServerSettingsPatch.providerInstances", () => {
  it("treats providerInstances as an optional whole-map replacement", () => {
    const patch = decodeServerSettingsPatch({});
    expect(patch.providerInstances).toBeUndefined();

    const replacement = decodeServerSettingsPatch({
      providerInstances: {
        codex_personal: { driver: "codex", config: { homePath: "~/.codex" } },
      },
    });
    expect(replacement.providerInstances).toBeDefined();
    expect(replacement.providerInstances?.[ProviderInstanceId.make("codex_personal")]?.driver).toBe(
      "codex",
    );
  });

  it("preserves a fork-defined driver entry through patch decoding", () => {
    const patch = decodeServerSettingsPatch({
      providerInstances: {
        ollama_local: {
          driver: "ollama",
          config: { endpoint: "http://localhost:11434" },
        },
      },
    });
    const ollamaId = ProviderInstanceId.make("ollama_local");
    expect(patch.providerInstances?.[ollamaId]?.driver).toBe("ollama");
  });
});

describe("ServerSettingsPatch string normalization", () => {
  it("trims string settings while decoding patches", () => {
    const patch = decodeServerSettingsPatch({
      addProjectBaseDirectory: "  ~/Development  ",
      textGenerationModelSelection: { model: "  gpt-5.4-mini  " },
      providers: {
        codex: {
          binaryPath: "  /opt/homebrew/bin/codex  ",
          homePath: "  ~/.codex  ",
        },
      },
      providerInstances: {
        codex_personal: {
          driver: "  codex  ",
          displayName: "  Codex Personal  ",
          config: { homePath: "  ~/.codex-personal  " },
        },
      },
    });

    expect(patch.addProjectBaseDirectory).toBe("~/Development");
    expect(patch.textGenerationModelSelection?.model).toBe("gpt-5.4-mini");
    expect(patch.providers?.codex?.binaryPath).toBe("/opt/homebrew/bin/codex");
    expect(patch.providers?.codex?.homePath).toBe("~/.codex");
    expect(patch.providerInstances?.[ProviderInstanceId.make("codex_personal")]?.driver).toBe(
      "codex",
    );
    expect(patch.providerInstances?.[ProviderInstanceId.make("codex_personal")]?.displayName).toBe(
      "Codex Personal",
    );
    expect(patch.providerInstances?.[ProviderInstanceId.make("codex_personal")]?.config).toEqual({
      homePath: "  ~/.codex-personal  ",
    });
  });

  it("trims encoded server settings values before validation", () => {
    const defaultSettings = decodeServerSettings({});
    const encoded = encodeServerSettings({
      ...defaultSettings,
      addProjectBaseDirectory: "  ~/Development  ",
      providers: {
        ...defaultSettings.providers,
        codex: {
          ...defaultSettings.providers.codex,
          binaryPath: "  /opt/homebrew/bin/codex  ",
        },
      },
    });

    expect(encoded.addProjectBaseDirectory).toBe("~/Development");
    expect(encoded.providers?.codex?.binaryPath).toBe("/opt/homebrew/bin/codex");
  });
});

describe("provider settings schema helpers", () => {
  it("uses the binary fallback when a persisted path is empty", () => {
    expect(decodeCodexSettings({ binaryPath: "" }).binaryPath).toBe("codex");
  });

  it("uses cursor-agent as the Cursor binary fallback", () => {
    expect(decodeCursorSettings({})).toMatchObject({ enabled: true, binaryPath: "cursor-agent" });
    expect(decodeCursorSettings({ binaryPath: "" }).binaryPath).toBe("cursor-agent");
    expect(decodeCursorSettings({ enabled: false }).enabled).toBe(false);
  });

  it("omits ordering metadata when no order is configured", () => {
    const schema = makeProviderSettingsSchema({ label: Schema.String });

    expect(Schema.decodeUnknownSync(schema)({ label: "Local" })).toEqual({ label: "Local" });
    expect(Schema.resolveAnnotations(schema)?.providerSettingsFormSchema).toBeUndefined();
  });
});

describe("ServerSettingsError", () => {
  it("formats operation failures with optional provider context", () => {
    const cause = new Error("sensitive settings detail");
    const baseError = new ServerSettingsError({
      settingsPath: "/home/user/.config/bibcode/settings.json",
      operation: "read-file",
      cause,
    });
    const providerError = new ServerSettingsError({
      settingsPath: "/home/user/.config/bibcode/settings.json",
      operation: "read-secret",
      providerInstanceId: "codex_personal",
      environmentVariable: "OPENAI_API_KEY",
      cause,
    });

    expect(baseError.message).toBe(
      "Server settings read-file failed at /home/user/.config/bibcode/settings.json.",
    );
    expect(providerError.message).toBe(
      "Server settings read-secret failed for provider codex_personal and environment variable OPENAI_API_KEY at /home/user/.config/bibcode/settings.json.",
    );
    expect(providerError.message).not.toContain(cause.message);
  });
});

describe("WorktreeWorkspaceError", () => {
  it("decodes an actionable worktree workspace validation error", () => {
    const error = decodeWorktreeWorkspaceError({
      _tag: "WorktreeWorkspaceError",
      path: "relative/worktrees",
      failure: "relative_path",
      message: "Workspace must be an absolute directory on this host.",
    });
    expect(error.failure).toBe("relative_path");
    expect(error.message).toBe("Workspace must be an absolute directory on this host.");
  });
});

describe("ClientSettings browser link target", () => {
  it("opens links in the app by default and keeps an explicit choice", () => {
    expect(decodeClientSettings({}).browserLinkTarget).toBe("app");
    expect(DEFAULT_CLIENT_SETTINGS.browserLinkTarget).toBe("app");
    expect(decodeClientSettings({ browserLinkTarget: "system" }).browserLinkTarget).toBe("system");
    expect(decodeClientSettings({ browserLinkTarget: "bogus" }).browserLinkTarget).toBe("app");
    expect(decodeClientSettingsPatch({ browserLinkTarget: "system" })).toMatchObject({
      browserLinkTarget: "system",
    });
    expect(decodeClientSettingsPatch({})).not.toHaveProperty("browserLinkTarget");
    expect(() => decodeClientSettingsPatch({ browserLinkTarget: "tab" })).toThrow();
  });
});
