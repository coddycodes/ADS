import { describe, expect, it } from "vite-plus/test";
import {
  DEFAULT_MODEL,
  ProviderDriverKind,
  ProviderInstanceId,
  type ModelCapabilities,
  type ProviderOptionDescriptor,
} from "@coddy/contracts";

import {
  applyClaudePromptEffortPrefix,
  buildProviderOptionSelectionsFromDescriptors,
  createModelCapabilities,
  createModelSelection,
  getModelSelectionBooleanOptionValue,
  getModelSelectionOptionDescriptors,
  getModelSelectionOptionValue,
  getModelSelectionStringOptionValue,
  getProviderOptionBooleanSelectionValue,
  getProviderOptionCurrentLabel,
  getProviderOptionCurrentValue,
  getProviderCapabilityDescriptors,
  getProviderOptionDescriptors,
  getProviderOptionSelectionValue,
  getProviderOptionStringSelectionValue,
  isClaudeUltrathinkPrompt,
  normalizeModelSlug,
  resolvePromptInjectedEffort,
  resolveModelSlugForProvider,
  resolveSelectableModel,
  trimOrNull,
  type SelectableModelOption,
} from "./model.ts";
import { getFastModeDescriptor, getFastModeOffValue } from "./providerSessionDefaults.ts";

const CODEX = ProviderDriverKind.make("codex");
const CLAUDE = ProviderDriverKind.make("claudeAgent");
const UNKNOWN = ProviderDriverKind.make("madeupprovider");

const codexCaps: ModelCapabilities = createModelCapabilities({
  optionDescriptors: [
    {
      id: "reasoningEffort",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "xhigh", label: "Extra High" },
        { id: "high", label: "High", isDefault: true },
      ],
      currentValue: "high",
    },
    {
      id: "fastMode",
      label: "Fast Mode",
      type: "boolean",
    },
  ],
});

const claudeCaps: ModelCapabilities = createModelCapabilities({
  optionDescriptors: [
    {
      id: "effort",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "medium", label: "Medium" },
        { id: "high", label: "High", isDefault: true },
        { id: "ultrathink", label: "Ultrathink" },
      ],
      currentValue: "high",
      promptInjectedValues: ["ultrathink"],
    },
    {
      id: "contextWindow",
      label: "Context Window",
      type: "select",
      options: [
        { id: "200k", label: "200k" },
        { id: "1m", label: "1M", isDefault: true },
      ],
      currentValue: "1m",
    },
  ],
});

describe("descriptor helpers", () => {
  it("does not fabricate Codex fast mode", () => {
    expect(
      getProviderCapabilityDescriptors({
        provider: ProviderDriverKind.make("codex"),
        caps: createModelCapabilities({ optionDescriptors: [] }),
      }),
    ).toEqual([]);
  });

  it("normalizes selected Codex effort without fabricating a service tier", () => {
    expect(
      getProviderCapabilityDescriptors({
        provider: CODEX,
        caps: createModelCapabilities({ optionDescriptors: [] }),
        selections: [{ id: "reasoningEffort", value: "high" }],
      }),
    ).toEqual([expect.objectContaining({ id: "reasoningEffort", currentValue: "high" })]);
  });

  it("preserves an advertised Codex fast service tier", () => {
    const descriptors = getProviderCapabilityDescriptors({
      provider: ProviderDriverKind.make("codex"),
      caps: createModelCapabilities({
        optionDescriptors: [
          {
            id: "serviceTier",
            label: "Service Tier",
            type: "select",
            options: [
              { id: "default", label: "Standard", isDefault: true },
              { id: "fast", label: "Fast" },
            ],
          },
        ],
      }),
    });

    expect(getFastModeDescriptor(CODEX, descriptors)?.id).toBe("serviceTier");
    expect(getFastModeOffValue(CODEX, getFastModeDescriptor(CODEX, descriptors))).toBe("default");
  });

  it("uses an advertised non-default Codex tier as Fast off", () => {
    const descriptors = getProviderCapabilityDescriptors({
      provider: CODEX,
      caps: createModelCapabilities({
        optionDescriptors: [
          {
            id: "serviceTier",
            label: "Service Tier",
            type: "select",
            options: [
              { id: "fast", label: "Fast" },
              { id: "flex", label: "Flex" },
            ],
          },
        ],
      }),
      selections: [{ id: "serviceTier", value: "fast" }],
    });
    const descriptor = getFastModeDescriptor(CODEX, descriptors);

    expect(descriptor?.id).toBe("serviceTier");
    expect(getFastModeOffValue(CODEX, descriptor)).toBe("flex");
  });

  it("does not expose one-way Codex Fast metadata as a toggle", () => {
    const descriptors = getProviderCapabilityDescriptors({
      provider: CODEX,
      caps: createModelCapabilities({
        optionDescriptors: [
          {
            id: "serviceTier",
            label: "Service Tier",
            type: "select",
            options: [{ id: "fast", label: "Fast" }],
          },
        ],
      }),
    });

    expect(getFastModeDescriptor(CODEX, descriptors)).toBeNull();
  });

  it("clones descriptor-owned arrays without mutating caller values", () => {
    const options = [{ id: "high", label: "High", isDefault: true }];
    const promptInjectedValues = ["ultrathink"];
    const input: ProviderOptionDescriptor = {
      id: "effort",
      label: "Reasoning",
      type: "select",
      options,
      promptInjectedValues,
    };

    const capabilities = createModelCapabilities({ optionDescriptors: [input] });
    const descriptor = capabilities.optionDescriptors?.[0];
    expect(descriptor).not.toBe(input);
    if (descriptor?.type === "select") {
      expect(descriptor.options).toEqual(options);
      expect(descriptor.options).not.toBe(options);
      expect(descriptor.promptInjectedValues).toEqual(promptInjectedValues);
      expect(descriptor.promptInjectedValues).not.toBe(promptInjectedValues);
    }
  });

  it("applies selection values to capability descriptors", () => {
    expect(
      getProviderOptionDescriptors({
        caps: claudeCaps,
        selections: [
          { id: "effort", value: "medium" },
          { id: "contextWindow", value: "200k" },
        ],
      }),
    ).toEqual([
      {
        id: "effort",
        label: "Reasoning",
        type: "select",
        options: [
          { id: "medium", label: "Medium" },
          { id: "high", label: "High", isDefault: true },
          { id: "ultrathink", label: "Ultrathink" },
        ],
        currentValue: "medium",
        promptInjectedValues: ["ultrathink"],
      },
      {
        id: "contextWindow",
        label: "Context Window",
        type: "select",
        options: [
          { id: "200k", label: "200k" },
          { id: "1m", label: "1M", isDefault: true },
        ],
        currentValue: "200k",
      },
    ]);
  });

  it("builds wire-format option selections from descriptors", () => {
    const descriptors = getProviderOptionDescriptors({
      caps: codexCaps,
      selections: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });

    expect(buildProviderOptionSelectionsFromDescriptors(descriptors)).toEqual([
      { id: "reasoningEffort", value: "high" },
      { id: "fastMode", value: true },
    ]);
  });

  it("returns no wire selections for missing descriptors or descriptors without values", () => {
    expect(buildProviderOptionSelectionsFromDescriptors(null)).toBeUndefined();
    expect(buildProviderOptionSelectionsFromDescriptors(undefined)).toBeUndefined();
    expect(buildProviderOptionSelectionsFromDescriptors([])).toBeUndefined();
    expect(
      buildProviderOptionSelectionsFromDescriptors([
        { id: "fast", label: "Fast", type: "boolean" },
        { id: "empty", label: "Empty", type: "select", options: [] },
      ]),
    ).toBeUndefined();
  });

  it("stores option selection arrays in model selections", () => {
    expect(
      createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ]),
    ).toEqual({
      instanceId: "codex",
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });
  });

  it("reads typed option selection values", () => {
    const selection = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", [
      { id: "reasoningEffort", value: "high" },
      { id: "fastMode", value: true },
    ]);

    expect(getProviderOptionStringSelectionValue(selection.options, "reasoningEffort")).toBe(
      "high",
    );
    expect(getProviderOptionStringSelectionValue(selection.options, "fastMode")).toBeUndefined();
    expect(getProviderOptionBooleanSelectionValue(selection.options, "fastMode")).toBe(true);
    expect(
      getProviderOptionBooleanSelectionValue(selection.options, "reasoningEffort"),
    ).toBeUndefined();
    expect(getModelSelectionStringOptionValue(selection, "reasoningEffort")).toBe("high");
    expect(getModelSelectionBooleanOptionValue(selection, "fastMode")).toBe(true);
  });
});

describe("selection value readers", () => {
  it("returns undefined for missing / nullish selection sources", () => {
    expect(getProviderOptionSelectionValue(null, "effort")).toBeUndefined();
    expect(getProviderOptionSelectionValue(undefined, "effort")).toBeUndefined();
    expect(getProviderOptionSelectionValue([], "effort")).toBeUndefined();
    expect(getProviderOptionStringSelectionValue(null, "effort")).toBeUndefined();
    expect(getProviderOptionBooleanSelectionValue(undefined, "effort")).toBeUndefined();
  });

  it("finds a selection value by id", () => {
    const selections = [
      { id: "effort", value: "high" },
      { id: "fast", value: true },
    ];
    expect(getProviderOptionSelectionValue(selections, "effort")).toBe("high");
    expect(getProviderOptionSelectionValue(selections, "fast")).toBe(true);
    expect(getProviderOptionSelectionValue(selections, "absent")).toBeUndefined();
  });

  it("reads model-selection option values through the options array", () => {
    const selection = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", [
      { id: "effort", value: "high" },
      { id: "fast", value: true },
    ]);
    expect(getModelSelectionOptionValue(selection, "effort")).toBe("high");
    expect(getModelSelectionOptionValue(selection, "fast")).toBe(true);
    expect(getModelSelectionOptionValue(null, "effort")).toBeUndefined();
    expect(getModelSelectionStringOptionValue(null, "effort")).toBeUndefined();
    expect(getModelSelectionBooleanOptionValue(undefined, "fast")).toBeUndefined();
  });
});

describe("getProviderOptionDescriptors edge cases", () => {
  it("returns no descriptors when capabilities omit them", () => {
    const caps: ModelCapabilities = {};
    expect(getProviderOptionDescriptors({ caps })).toEqual([]);
  });

  it("keeps the descriptor's own currentValue when no selection overrides it", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "effort",
          label: "Reasoning",
          type: "select",
          options: [
            { id: "medium", label: "Medium" },
            { id: "high", label: "High", isDefault: true },
          ],
          currentValue: "medium",
        },
      ],
    });
    const descriptor = getProviderOptionDescriptors({ caps })[0]!;
    expect(descriptor.type).toBe("select");
    if (descriptor.type === "select") {
      expect(descriptor.currentValue).toBe("medium");
    }
  });

  it("falls back to the default option when the descriptor has no currentValue", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "effort",
          label: "Reasoning",
          type: "select",
          options: [
            { id: "medium", label: "Medium" },
            { id: "high", label: "High", isDefault: true },
          ],
        },
      ],
    });
    const descriptor = getProviderOptionDescriptors({ caps })[0]!;
    if (descriptor.type === "select") {
      expect(descriptor.currentValue).toBe("high");
    }
  });

  it("resets a prompt-injected selection back to the default option", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "effort",
          label: "Reasoning",
          type: "select",
          options: [
            { id: "high", label: "High", isDefault: true },
            { id: "ultrathink", label: "Ultrathink" },
          ],
          promptInjectedValues: ["ultrathink"],
        },
      ],
    });
    const descriptor = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "effort", value: "ultrathink" }],
    })[0]!;
    if (descriptor.type === "select") {
      expect(descriptor.currentValue).toBe("high");
    }
  });

  it("preserves a prompt-injected selection only when explicitly requested", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "effort",
          label: "Reasoning",
          type: "select",
          options: [
            { id: "high", label: "High", isDefault: true },
            { id: "ultrathink", label: "Ultrathink" },
          ],
          promptInjectedValues: ["ultrathink"],
        },
      ],
    });
    const descriptor = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "effort", value: "ultrathink" }],
      preservePromptInjectedSelections: true,
    })[0]!;
    if (descriptor.type === "select") {
      expect(descriptor.currentValue).toBe("ultrathink");
    }
  });

  it("drops a prompt-injected value when no default exists", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "effort",
          label: "Reasoning",
          type: "select",
          options: [{ id: "ultrathink", label: "Ultrathink" }],
          promptInjectedValues: ["ultrathink"],
        },
      ],
    });
    const descriptor = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "effort", value: "ultrathink" }],
    })[0]!;
    expect(descriptor.type).toBe("select");
    expect("currentValue" in descriptor).toBe(false);
  });

  it("falls back from invalid select values to a current value or default", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [
        {
          id: "withCurrent",
          label: "With current",
          type: "select",
          options: [{ id: "current", label: "Current" }],
          currentValue: "current",
        },
        {
          id: "withDefault",
          label: "With default",
          type: "select",
          options: [{ id: "default", label: "Default", isDefault: true }],
        },
      ],
    });
    const descriptors = getProviderOptionDescriptors({
      caps,
      selections: [
        { id: "withCurrent", value: "invalid" },
        { id: "withDefault", value: "invalid" },
      ],
    });
    expect(getProviderOptionCurrentValue(descriptors[0])).toBe("current");
    expect(getProviderOptionCurrentValue(descriptors[1])).toBe("default");
  });

  it("passes through an arbitrary value for a select with no options", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [{ id: "effort", label: "Reasoning", type: "select", options: [] }],
    });
    const descriptor = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "effort", value: "whatever" }],
    })[0]!;
    if (descriptor.type === "select") {
      expect(descriptor.currentValue).toBe("whatever");
    }
  });

  it("drops currentValue when a select cannot resolve any value", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [{ id: "effort", label: "Reasoning", type: "select", options: [] }],
    });
    const descriptor = getProviderOptionDescriptors({ caps })[0]!;
    expect(descriptor.type).toBe("select");
    expect("currentValue" in descriptor).toBe(false);
  });

  it("applies a boolean selection value and leaves it alone for non-boolean input", () => {
    const caps = createModelCapabilities({
      optionDescriptors: [{ id: "fast", label: "Fast", type: "boolean" }],
    });
    const applied = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "fast", value: true }],
    })[0]!;
    if (applied.type === "boolean") {
      expect(applied.currentValue).toBe(true);
    }
    // A string selection value cannot apply to a boolean descriptor.
    const unchanged = getProviderOptionDescriptors({
      caps,
      selections: [{ id: "fast", value: "nope" }],
    })[0]!;
    if (unchanged.type === "boolean") {
      expect(unchanged.currentValue).toBeUndefined();
    }
  });
});

describe("getProviderOptionCurrentValue / Label", () => {
  const selectDescriptor: ProviderOptionDescriptor = {
    id: "effort",
    label: "Reasoning",
    type: "select",
    options: [
      { id: "medium", label: "Medium" },
      { id: "high", label: "High", isDefault: true },
    ],
    currentValue: "medium",
  };
  const selectNoCurrent: ProviderOptionDescriptor = {
    id: "effort",
    label: "Reasoning",
    type: "select",
    options: [
      { id: "medium", label: "Medium" },
      { id: "high", label: "High", isDefault: true },
    ],
  };
  const booleanOn: ProviderOptionDescriptor = {
    id: "fast",
    label: "Fast",
    type: "boolean",
    currentValue: true,
  };
  const booleanUnset: ProviderOptionDescriptor = {
    id: "fast",
    label: "Fast",
    type: "boolean",
  };

  it("resolves current values", () => {
    expect(getProviderOptionCurrentValue(null)).toBeUndefined();
    expect(getProviderOptionCurrentValue(undefined)).toBeUndefined();
    expect(getProviderOptionCurrentValue(selectDescriptor)).toBe("medium");
    expect(getProviderOptionCurrentValue(selectNoCurrent)).toBe("high");
    expect(getProviderOptionCurrentValue(booleanOn)).toBe(true);
    expect(getProviderOptionCurrentValue(booleanUnset)).toBeUndefined();
  });

  it("resolves current labels", () => {
    expect(getProviderOptionCurrentLabel(null)).toBeUndefined();
    expect(getProviderOptionCurrentLabel(selectDescriptor)).toBe("Medium");
    expect(getProviderOptionCurrentLabel(selectNoCurrent)).toBe("High");
    expect(getProviderOptionCurrentLabel(booleanOn)).toBe("On");
    expect(
      getProviderOptionCurrentLabel({
        id: "fast",
        label: "Fast",
        type: "boolean",
        currentValue: false,
      }),
    ).toBe("Off");
    expect(getProviderOptionCurrentLabel(booleanUnset)).toBeUndefined();
  });

  it("returns undefined labels for selects with no matching or non-string current value", () => {
    const emptyOptions: ProviderOptionDescriptor = {
      id: "effort",
      label: "Reasoning",
      type: "select",
      options: [],
    };
    // No options, no currentValue → current value is undefined → label undefined.
    expect(getProviderOptionCurrentLabel(emptyOptions)).toBeUndefined();
    const staleCurrent: ProviderOptionDescriptor = {
      id: "effort",
      label: "Reasoning",
      type: "select",
      options: [{ id: "high", label: "High" }],
      currentValue: "gone",
    };
    // currentValue does not match any option → find returns undefined.
    expect(getProviderOptionCurrentLabel(staleCurrent)).toBeUndefined();
  });
});

describe("getModelSelectionOptionDescriptors", () => {
  const caps = createModelCapabilities({
    optionDescriptors: [
      {
        id: "effort",
        label: "Reasoning",
        type: "select",
        options: [{ id: "high", label: "High", isDefault: true }],
      },
    ],
  });

  it("returns [] without a model selection or caps", () => {
    expect(getModelSelectionOptionDescriptors(null, caps)).toEqual([]);
    const selection = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4");
    expect(getModelSelectionOptionDescriptors(selection, null)).toEqual([]);
    expect(getModelSelectionOptionDescriptors(selection, undefined)).toEqual([]);
  });

  it("resolves descriptors from the selection's options", () => {
    const selection = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", [
      { id: "effort", value: "high" },
    ]);
    const descriptors = getModelSelectionOptionDescriptors(selection, caps);
    expect(descriptors).toHaveLength(1);
    expect(descriptors[0]?.id).toBe("effort");
  });
});

describe("createModelSelection", () => {
  it("omits the options key when no selections are supplied", () => {
    expect(createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4")).toEqual({
      instanceId: "codex",
      model: "gpt-5.4",
    });
    expect(createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", null)).toEqual({
      instanceId: "codex",
      model: "gpt-5.4",
    });
    expect(createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", [])).toEqual({
      instanceId: "codex",
      model: "gpt-5.4",
    });
  });

  it("clones caller-owned selections", () => {
    const options = [{ id: "effort", value: "high" }];
    const selection = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4", options);
    expect(selection.options).toEqual(options);
    expect(selection.options).not.toBe(options);
    expect(selection.options?.[0]).not.toBe(options[0]);
  });
});

describe("trimOrNull", () => {
  it("returns null for non-strings and empty strings", () => {
    expect(trimOrNull(null)).toBeNull();
    expect(trimOrNull(undefined)).toBeNull();
    expect(trimOrNull("   ")).toBeNull();
  });

  it("trims and returns non-empty strings", () => {
    expect(trimOrNull("  hi  ")).toBe("hi");
  });
});

describe("isClaudeUltrathinkPrompt", () => {
  it("detects the ultrathink keyword regardless of case", () => {
    expect(isClaudeUltrathinkPrompt("please ULTRATHINK this")).toBe(true);
    expect(isClaudeUltrathinkPrompt("ultrathink")).toBe(true);
  });

  it("returns false for missing or unrelated text", () => {
    expect(isClaudeUltrathinkPrompt(null)).toBe(false);
    expect(isClaudeUltrathinkPrompt(undefined)).toBe(false);
    expect(isClaudeUltrathinkPrompt("just thinking")).toBe(false);
    expect(isClaudeUltrathinkPrompt("ultrathinking")).toBe(false);
  });
});

describe("normalizeModelSlug", () => {
  it("returns null for non-string / empty input", () => {
    expect(normalizeModelSlug(null)).toBeNull();
    expect(normalizeModelSlug(undefined)).toBeNull();
    expect(normalizeModelSlug("   ")).toBeNull();
  });

  it("maps aliases for the default (codex) provider", () => {
    expect(normalizeModelSlug("gpt-5-codex")).toBe("gpt-5.4");
    expect(normalizeModelSlug("5.3")).toBe("gpt-5.3-codex");
  });

  it("passes through unknown slugs and canonical slugs unchanged", () => {
    expect(normalizeModelSlug("gpt-5.4")).toBe("gpt-5.4");
    expect(normalizeModelSlug("some-custom-model")).toBe("some-custom-model");
  });

  it("uses the provider-specific alias table", () => {
    expect(normalizeModelSlug("opus", CLAUDE)).toBe("claude-opus-5");
    expect(normalizeModelSlug("sonnet", CLAUDE)).toBe("claude-sonnet-5");
  });

  it("passes slugs through for providers without an alias table", () => {
    expect(normalizeModelSlug("weird-model", UNKNOWN)).toBe("weird-model");
  });
});

describe("resolveModelSlugForProvider", () => {
  it("returns the normalized slug when resolvable", () => {
    expect(resolveModelSlugForProvider(CLAUDE, "opus")).toBe("claude-opus-5");
    expect(resolveModelSlugForProvider(CODEX, "gpt-5-codex")).toBe("gpt-5.4");
  });

  it("falls back to the provider default when the model is empty", () => {
    expect(resolveModelSlugForProvider(CLAUDE, null)).toBe("claude-sonnet-5");
    expect(resolveModelSlugForProvider(CODEX, "")).toBe("gpt-5.4");
  });

  it("falls back to the global default for an unknown provider", () => {
    expect(resolveModelSlugForProvider(UNKNOWN, null)).toBe(DEFAULT_MODEL);
  });
});

describe("resolveSelectableModel", () => {
  const options: ReadonlyArray<SelectableModelOption> = [
    { slug: "gpt-5.4", name: "GPT 5.4" },
    { slug: "gpt-5.3-codex", name: "GPT 5.3 Codex" },
  ];

  it("returns null for non-string / empty values", () => {
    expect(resolveSelectableModel(CODEX, null, options)).toBeNull();
    expect(resolveSelectableModel(CODEX, undefined, options)).toBeNull();
    expect(resolveSelectableModel(CODEX, "   ", options)).toBeNull();
  });

  it("matches directly by slug", () => {
    expect(resolveSelectableModel(CODEX, "gpt-5.4", options)).toBe("gpt-5.4");
  });

  it("matches by display name case-insensitively", () => {
    expect(resolveSelectableModel(CODEX, "gpt 5.4", options)).toBe("gpt-5.4");
  });

  it("matches through the provider alias table", () => {
    expect(resolveSelectableModel(CODEX, "gpt-5-codex", options)).toBe("gpt-5.4");
    expect(resolveSelectableModel(CODEX, "5.3", options)).toBe("gpt-5.3-codex");
  });

  it("returns the server selector when a persisted canonical slug has the same identity", () => {
    const discoveredOptions: ReadonlyArray<SelectableModelOption> = [
      { slug: "opus", name: "Opus" },
      { slug: "sonnet", name: "Sonnet" },
    ];

    expect(resolveSelectableModel(CLAUDE, "claude-sonnet-5", discoveredOptions)).toBe("sonnet");
  });

  it("returns null when nothing resolves to an available option", () => {
    expect(resolveSelectableModel(CODEX, "definitely-not-a-model", options)).toBeNull();
  });
});

describe("resolvePromptInjectedEffort", () => {
  const caps = createModelCapabilities({
    optionDescriptors: [
      {
        id: "effort",
        label: "Reasoning",
        type: "select",
        options: [
          { id: "high", label: "High", isDefault: true },
          { id: "ultrathink", label: "Ultrathink" },
        ],
        promptInjectedValues: ["ultrathink"],
      },
      { id: "fast", label: "Fast", type: "boolean" },
    ],
  });

  it("returns null for empty effort", () => {
    expect(resolvePromptInjectedEffort(caps, null)).toBeNull();
    expect(resolvePromptInjectedEffort(caps, "   ")).toBeNull();
  });

  it("returns the effort when it is a prompt-injected value", () => {
    expect(resolvePromptInjectedEffort(caps, "ultrathink")).toBe("ultrathink");
  });

  it("returns null when the effort is not prompt-injected on any descriptor", () => {
    expect(resolvePromptInjectedEffort(caps, "high")).toBeNull();
  });
});

describe("applyClaudePromptEffortPrefix", () => {
  it("returns the trimmed text for empty input", () => {
    expect(applyClaudePromptEffortPrefix("   ", "ultrathink")).toBe("");
  });

  it("leaves the text unchanged when effort is not ultrathink", () => {
    expect(applyClaudePromptEffortPrefix("Fix the bug", "high")).toBe("Fix the bug");
    expect(applyClaudePromptEffortPrefix("  Fix the bug  ", null)).toBe("Fix the bug");
  });

  it("prepends the Ultrathink prefix once", () => {
    expect(applyClaudePromptEffortPrefix("Fix the bug", "ultrathink")).toBe(
      "Ultrathink:\nFix the bug",
    );
  });

  it("does not double-prefix already-prefixed text", () => {
    expect(applyClaudePromptEffortPrefix("Ultrathink: already", "ultrathink")).toBe(
      "Ultrathink: already",
    );
  });
});
