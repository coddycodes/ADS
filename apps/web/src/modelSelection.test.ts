import { ProviderDriverKind, ProviderInstanceId, type ServerProvider } from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS, type UnifiedSettings } from "@coddy/contracts/settings";
import { describe, expect, it } from "vite-plus/test";
import { deriveProviderInstanceEntries } from "./providerInstances";
import {
  getAppModelOptions,
  getAppModelOptionsForInstance,
  getCustomModelOptionsByInstance,
  MAX_CUSTOM_MODEL_LENGTH,
  normalizeCustomModelSlugs,
  resolveAppModelSelection,
  resolveAppModelSelectionForInstance,
  resolveAppModelSelectionState,
} from "./modelSelection";

function provider(input: {
  provider?: ProviderDriverKind;
  instanceId: string;
  models?: ReadonlyArray<string>;
}): ServerProvider {
  const driver =
    input.provider ??
    (input.instanceId.startsWith("claude_")
      ? ProviderDriverKind.make("claudeAgent")
      : ProviderDriverKind.make("codex"));
  return {
    instanceId: ProviderInstanceId.make(input.instanceId),
    driver,
    enabled: true,
    installed: true,
    version: null,
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-01-01T00:00:00.000Z",
    models: (input.models ?? []).map((slug) => ({
      slug,
      name: slug,
      isCustom: false,
      capabilities: {},
    })),
    slashCommands: [],
    skills: [],
    agents: [],
  };
}

function settingsWithProviderInstances(): UnifiedSettings {
  return {
    ...DEFAULT_UNIFIED_SETTINGS,
    providerInstances: {
      [ProviderInstanceId.make("claudeAgent")]: {
        driver: ProviderDriverKind.make("claudeAgent"),
        config: { customModels: [] },
      },
      [ProviderInstanceId.make("claude_openrouter")]: {
        driver: ProviderDriverKind.make("claudeAgent"),
        config: { customModels: ["openai/gpt-5.5"] },
      },
    },
  };
}

describe("instance-scoped model selection", () => {
  it("keeps custom models on the provider instance that declared them", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-sonnet-4-6"],
      }),
      provider({
        instanceId: "claude_openrouter",
        models: ["claude-sonnet-4-6"],
      }),
    ];
    const entries = deriveProviderInstanceEntries(providers);
    const stock = entries.find((entry) => entry.instanceId === "claudeAgent")!;
    const openrouter = entries.find((entry) => entry.instanceId === "claude_openrouter")!;

    expect(
      getAppModelOptionsForInstance(settingsWithProviderInstances(), stock).map(
        (option) => option.slug,
      ),
    ).not.toContain("openai/gpt-5.5");
    expect(
      getAppModelOptionsForInstance(settingsWithProviderInstances(), openrouter).map(
        (option) => option.slug,
      ),
    ).toContain("openai/gpt-5.5");
  });

  it("resolves a custom slug against the selected custom instance", () => {
    const providers = [
      provider({ provider: ProviderDriverKind.make("claudeAgent"), instanceId: "claudeAgent" }),
      provider({
        provider: ProviderDriverKind.make("claudeAgent"),
        instanceId: "claude_openrouter",
      }),
    ];

    expect(
      resolveAppModelSelectionForInstance(
        ProviderInstanceId.make("claude_openrouter"),
        settingsWithProviderInstances(),
        providers,
        "openai/gpt-5.5",
      ),
    ).toBe("openai/gpt-5.5");
  });

  it("includes Grok custom models from the selected provider instance", () => {
    const providers = [provider({ provider: ProviderDriverKind.make("grok"), instanceId: "grok" })];
    const settings: UnifiedSettings = {
      ...settingsWithProviderInstances(),
      providerInstances: {
        ...settingsWithProviderInstances().providerInstances,
        [ProviderInstanceId.make("grok")]: {
          driver: ProviderDriverKind.make("grok"),
          config: { customModels: ["grok-test-custom-model"] },
        },
      },
    };
    const grok = deriveProviderInstanceEntries(providers).find(
      (entry) => entry.instanceId === "grok",
    )!;

    expect(getAppModelOptionsForInstance(settings, grok).map((option) => option.slug)).toContain(
      "grok-test-custom-model",
    );
  });

  it("does not inject an unknown selected slug into the stock instance list", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-sonnet-4-6"],
      }),
      provider({
        instanceId: "claude_openrouter",
        models: ["claude-sonnet-4-6"],
      }),
    ];
    const stock = deriveProviderInstanceEntries(providers).find(
      (entry) => entry.instanceId === "claudeAgent",
    )!;

    expect(
      getAppModelOptionsForInstance(settingsWithProviderInstances(), stock).map(
        (option) => option.slug,
      ),
    ).not.toContain("openai/gpt-5.5");
  });

  it("hides server models from the instance option list", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-opus-4-6", "claude-sonnet-4-6"],
      }),
    ];
    const settings: UnifiedSettings = {
      ...settingsWithProviderInstances(),
      providerModelPreferences: {
        [ProviderInstanceId.make("claudeAgent")]: {
          hiddenModels: ["claude-opus-4-6"],
          modelOrder: [],
        },
      },
    };
    const stock = deriveProviderInstanceEntries(providers).find(
      (entry) => entry.instanceId === "claudeAgent",
    )!;

    expect(getAppModelOptionsForInstance(settings, stock).map((option) => option.slug)).toEqual([
      "claude-sonnet-4-6",
    ]);
  });

  it("applies persisted per-instance model ordering", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
      }),
    ];
    const settings: UnifiedSettings = {
      ...settingsWithProviderInstances(),
      providerModelPreferences: {
        [ProviderInstanceId.make("claudeAgent")]: {
          hiddenModels: [],
          modelOrder: ["claude-haiku-4-5", "claude-opus-4-6"],
        },
      },
    };
    const stock = deriveProviderInstanceEntries(providers).find(
      (entry) => entry.instanceId === "claudeAgent",
    )!;

    expect(getAppModelOptionsForInstance(settings, stock).map((option) => option.slug)).toEqual([
      "claude-haiku-4-5",
      "claude-opus-4-6",
      "claude-sonnet-4-6",
    ]);
  });

  it("falls back when the selected model is hidden", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-opus-4-6", "claude-sonnet-4-6"],
      }),
    ];
    const settings: UnifiedSettings = {
      ...settingsWithProviderInstances(),
      providerModelPreferences: {
        [ProviderInstanceId.make("claudeAgent")]: {
          hiddenModels: ["claude-opus-4-6"],
          modelOrder: [],
        },
      },
    };

    expect(
      resolveAppModelSelectionForInstance(
        ProviderInstanceId.make("claudeAgent"),
        settings,
        providers,
        "claude-opus-4-6",
      ),
    ).toBe("claude-sonnet-4-6");
  });

  it("falls back instead of resolving a custom slug against the wrong instance", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-sonnet-4-6"],
      }),
      provider({
        instanceId: "claude_openrouter",
        models: ["claude-sonnet-4-6"],
      }),
    ];

    expect(
      resolveAppModelSelectionForInstance(
        ProviderInstanceId.make("claudeAgent"),
        settingsWithProviderInstances(),
        providers,
        "openai/gpt-5.5",
      ),
    ).toBe("claude-sonnet-4-6");
  });

  it("preserves custom provider instances in settings model selection", () => {
    const providers = [
      provider({
        instanceId: "claudeAgent",
        models: ["claude-sonnet-4-6"],
      }),
      provider({
        instanceId: "claude_openrouter",
        models: ["claude-sonnet-4-6"],
      }),
    ];
    const settings: UnifiedSettings = {
      ...settingsWithProviderInstances(),
      textGenerationModelSelection: {
        instanceId: ProviderInstanceId.make("claude_openrouter"),
        model: "openai/gpt-5.5",
      },
    };

    expect(resolveAppModelSelectionState(settings, providers)).toEqual({
      instanceId: ProviderInstanceId.make("claude_openrouter"),
      model: "openai/gpt-5.5",
    });
  });

  it("normalizes, de-duplicates, bounds, and filters custom model slugs", () => {
    const tooLong = "x".repeat(MAX_CUSTOM_MODEL_LENGTH + 1);
    const candidates = [
      null,
      undefined,
      " ",
      "custom-one",
      " custom-one ",
      "built-in",
      tooLong,
      ...Array.from({ length: 40 }, (_, index) => `custom-${index + 2}`),
    ];

    const result = normalizeCustomModelSlugs(candidates, new Set(["built-in"]));

    expect(result).toHaveLength(32);
    expect(result[0]).toBe("custom-one");
    expect(result).not.toContain("built-in");
    expect(result).not.toContain(tooLong);
  });

  it("builds default-instance options from legacy settings and model metadata", () => {
    const providers = [
      {
        ...provider({ instanceId: "codex", models: [] }),
        models: [
          {
            slug: "built-in",
            name: "Built in",
            shortName: "Built",
            subProvider: "openai",
            isCustom: false,
            capabilities: {},
          },
          {
            slug: "server-custom",
            name: "Server custom",
            isCustom: true,
            capabilities: {},
          },
        ],
      },
    ];
    const settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      providerInstances: {},
      providers: {
        ...DEFAULT_UNIFIED_SETTINGS.providers,
        codex: { ...DEFAULT_UNIFIED_SETTINGS.providers.codex, customModels: ["local-custom"] },
      },
    };

    expect(getAppModelOptions(settings, providers, ProviderDriverKind.make("codex"))).toEqual([
      {
        slug: "built-in",
        name: "Built in",
        shortName: "Built",
        subProvider: "openai",
        isCustom: false,
      },
      { slug: "server-custom", name: "Server custom", isCustom: true },
      { slug: "local-custom", name: "local-custom", isCustom: true },
    ]);
    expect(
      resolveAppModelSelection(
        ProviderDriverKind.make("codex"),
        settings,
        providers,
        "local-custom",
      ),
    ).toBe("local-custom");
  });

  it("ignores malformed instance custom-model config and legacy models on custom slots", () => {
    const providers = [
      provider({ instanceId: "claudeAgent", models: ["claude-sonnet-4-6"] }),
      provider({ instanceId: "claude_custom", models: ["claude-sonnet-4-6"] }),
    ];
    const entries = deriveProviderInstanceEntries(providers);
    const stock = entries.find((entry) => entry.instanceId === "claudeAgent")!;
    const custom = entries.find((entry) => entry.instanceId === "claude_custom")!;
    const settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      providerInstances: {
        [ProviderInstanceId.make("claudeAgent")]: {
          driver: ProviderDriverKind.make("claudeAgent"),
          config: null,
        },
        [ProviderInstanceId.make("claude_custom")]: {
          driver: ProviderDriverKind.make("claudeAgent"),
          config: { customModels: "not-an-array" },
        },
      },
      providers: {
        ...DEFAULT_UNIFIED_SETTINGS.providers,
        claudeAgent: {
          ...DEFAULT_UNIFIED_SETTINGS.providers.claudeAgent,
          customModels: ["openai/gpt-legacy"],
        },
      },
    } as UnifiedSettings;

    expect(getAppModelOptionsForInstance(settings, stock).map((option) => option.slug)).toEqual([
      "claude-sonnet-4-6",
    ]);
    expect(
      getAppModelOptionsForInstance(settings, custom).map((option) => option.slug),
    ).not.toContain("openai/gpt-legacy");
  });

  it("returns null for missing instances and maps options for every present instance", () => {
    const providers = [
      provider({ instanceId: "codex", models: ["gpt-test"] }),
      provider({ instanceId: "claudeAgent", models: ["claude-test"] }),
    ];

    expect(
      resolveAppModelSelectionForInstance(
        ProviderInstanceId.make("missing"),
        DEFAULT_UNIFIED_SETTINGS,
        providers,
        null,
      ),
    ).toBeNull();
    expect(getCustomModelOptionsByInstance(DEFAULT_UNIFIED_SETTINGS, providers).size).toBe(2);
  });

  it("falls back from unavailable selected instances and legacy empty provider lists", () => {
    const unavailable = {
      ...provider({ instanceId: "codex", models: ["gpt-selected"] }),
      enabled: false,
    };
    const fallback = provider({ instanceId: "claudeAgent", models: ["claude-fallback"] });
    const settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      textGenerationModelSelection: {
        instanceId: ProviderInstanceId.make("codex"),
        model: "gpt-selected",
        options: [{ id: "fast", value: true }],
      },
    };

    expect(resolveAppModelSelectionState(settings, [unavailable, fallback])).toMatchObject({
      instanceId: "claudeAgent",
      model: "claude-fallback",
    });
    expect(resolveAppModelSelectionState(DEFAULT_UNIFIED_SETTINGS, [])).toMatchObject({
      instanceId: "codex",
    });
  });

  it("covers duplicate custom models and empty instance fallbacks", () => {
    const serverCustom = {
      ...provider({ instanceId: "codex", models: [] }),
      models: [
        {
          slug: "server-custom",
          name: "Server custom",
          isCustom: true,
          capabilities: {},
        },
      ],
    } satisfies ServerProvider;
    const duplicateSettings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      providerInstances: {
        [ProviderInstanceId.make("codex")]: {
          driver: ProviderDriverKind.make("codex"),
          config: { customModels: ["server-custom", 123, "local-custom"] },
        },
      },
    } as unknown as UnifiedSettings;
    expect(
      getAppModelOptionsForInstance(
        duplicateSettings,
        deriveProviderInstanceEntries([serverCustom])[0]!,
      ).map((option) => option.slug),
    ).toEqual(["server-custom", "local-custom"]);

    const empty = provider({ instanceId: "codex", models: [] });
    expect(
      resolveAppModelSelectionForInstance(
        empty.instanceId,
        {
          ...DEFAULT_UNIFIED_SETTINGS,
          providerInstances: {},
          providers: {} as UnifiedSettings["providers"],
        },
        [empty],
        null,
      ),
    ).toBeNull();
  });

  it("uses default selections and handles providers without a default model", () => {
    const noSelection = {
      ...DEFAULT_UNIFIED_SETTINGS,
      textGenerationModelSelection: undefined,
    } as unknown as UnifiedSettings;
    expect(resolveAppModelSelectionState(noSelection, [])).toMatchObject({ instanceId: "codex" });

    const custom = provider({
      provider: ProviderDriverKind.make("custom"),
      instanceId: "custom",
      models: [],
    });
    expect(
      resolveAppModelSelectionState(
        {
          ...noSelection,
          providerInstances: {},
          providers: {} as UnifiedSettings["providers"],
        },
        [custom],
      ),
    ).toEqual({ instanceId: ProviderInstanceId.make("custom"), model: "" });
  });
});
