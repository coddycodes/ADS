import {
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProvider,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { resolveProviderSessionSelectionForInstance } from "./providerSessionSelection";

describe("resolveProviderSessionSelectionForInstance", () => {
  it("resolves a built-in provider's shared model and options without changing provider routing", () => {
    const instanceId = ProviderInstanceId.make("codex");
    const provider: ServerProvider = {
      instanceId,
      driver: ProviderDriverKind.make("codex"),
      enabled: true,
      installed: true,
      version: "1.0.0",
      status: "ready",
      auth: { status: "authenticated" },
      checkedAt: "2026-07-20T00:00:00.000Z",
      models: [
        {
          slug: "gpt-5.4",
          name: "GPT-5.4",
          isCustom: false,
          capabilities: {
            optionDescriptors: [
              {
                id: "reasoningEffort",
                label: "Reasoning",
                type: "select",
                options: [
                  { id: "medium", label: "Medium", isDefault: true },
                  { id: "high", label: "High" },
                ],
                currentValue: "medium",
              },
              {
                id: "serviceTier",
                label: "Service tier",
                type: "select",
                options: [
                  { id: "default", label: "Default", isDefault: true },
                  { id: "fast", label: "Fast" },
                ],
                currentValue: "default",
              },
            ],
          },
        },
      ],
      slashCommands: [],
      skills: [],
      agents: [],
    };

    expect(
      resolveProviderSessionSelectionForInstance({
        instanceId,
        providers: [provider],
        settings: {
          ...DEFAULT_SERVER_SETTINGS,
          providerSessionDefaults: {
            [ProviderDriverKind.make("codex")]: {
              model: "gpt-5.4",
              options: [
                { id: "reasoningEffort", value: "high" },
                { id: "serviceTier", value: "fast" },
              ],
            },
          },
        },
      }).modelSelection,
    ).toEqual({
      instanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });
});
