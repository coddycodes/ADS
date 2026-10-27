import {
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
} from "@coddy/contracts";
import * as Duration from "effect/Duration";
import { describe, expect, it } from "vite-plus/test";
import { createModelSelection } from "./model.ts";
import { applyServerSettingsPatch } from "./serverSettings.ts";

describe("serverSettings helpers", () => {
  it("patches terminal activity without changing Chat activity", () => {
    expect(
      applyServerSettingsPatch(DEFAULT_SERVER_SETTINGS, {
        enableTerminalAgentActivity: true,
      }),
    ).toMatchObject({
      enableChatAgentActivity: true,
      enableTerminalAgentActivity: true,
    });
  });

  it("applies ordinary nested patches and explicit automatic fetch intervals", () => {
    const interval = Duration.seconds(60);
    const next = applyServerSettingsPatch(DEFAULT_SERVER_SETTINGS, {
      enableAssistantStreaming: true,
      automaticGitFetchInterval: interval,
    });
    expect(next.enableAssistantStreaming).toBe(true);
    expect(next.automaticGitFetchInterval).toEqual(interval);
    expect(DEFAULT_SERVER_SETTINGS.enableAssistantStreaming).toBe(false);
  });

  it("replaces text generation selection when provider/model are provided", () => {
    const current = {
      ...DEFAULT_SERVER_SETTINGS,
      textGenerationModelSelection: createModelSelection(
        ProviderInstanceId.make("codex"),
        "gpt-5.4-mini",
        [
          { id: "reasoningEffort", value: "high" },
          { id: "fastMode", value: true },
        ],
      ),
    };

    expect(
      applyServerSettingsPatch(current, {
        textGenerationModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "gpt-5.4-mini",
        },
      }).textGenerationModelSelection,
    ).toEqual({
      instanceId: "codex",
      model: "gpt-5.4-mini",
    });
  });

  it("still deep merges text generation selection when only options are provided", () => {
    const current = {
      ...DEFAULT_SERVER_SETTINGS,
      textGenerationModelSelection: createModelSelection(
        ProviderInstanceId.make("codex"),
        "gpt-5.4-mini",
        [
          { id: "reasoningEffort", value: "high" },
          { id: "fastMode", value: true },
        ],
      ),
    };

    expect(
      applyServerSettingsPatch(current, {
        textGenerationModelSelection: {
          options: [{ id: "fastMode", value: false }],
        },
      }).textGenerationModelSelection,
    ).toEqual({
      instanceId: "codex",
      model: "gpt-5.4-mini",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: false },
      ],
    });
  });

  it("clones unchanged options for an empty selection patch", () => {
    const current = {
      ...DEFAULT_SERVER_SETTINGS,
      textGenerationModelSelection: createModelSelection(
        ProviderInstanceId.make("codex"),
        "gpt-5.4-mini",
        [{ id: "reasoningEffort", value: "high" }],
      ),
    };
    const next = applyServerSettingsPatch(current, { textGenerationModelSelection: {} });
    expect(next.textGenerationModelSelection).toEqual(current.textGenerationModelSelection);
    expect(next.textGenerationModelSelection.options).not.toBe(
      current.textGenerationModelSelection.options,
    );
  });

  it("handles empty and newly introduced option lists", () => {
    const withoutOptions = createModelSelection(ProviderInstanceId.make("codex"), "gpt-5.4-mini");
    const current = { ...DEFAULT_SERVER_SETTINGS, textGenerationModelSelection: withoutOptions };

    expect(
      applyServerSettingsPatch(current, { textGenerationModelSelection: {} })
        .textGenerationModelSelection,
    ).toEqual(withoutOptions);
    expect(
      applyServerSettingsPatch(current, {
        textGenerationModelSelection: { options: [{ id: "fastMode", value: true }] },
      }).textGenerationModelSelection,
    ).toEqual({ ...withoutOptions, options: [{ id: "fastMode", value: true }] });

    const withOptions = {
      ...DEFAULT_SERVER_SETTINGS,
      textGenerationModelSelection: createModelSelection(
        ProviderInstanceId.make("codex"),
        "gpt-5.4-mini",
        [{ id: "fastMode", value: true }],
      ),
    };
    expect(
      applyServerSettingsPatch(withOptions, {
        textGenerationModelSelection: { options: [] },
      }).textGenerationModelSelection,
    ).toEqual({ instanceId: "codex", model: "gpt-5.4-mini" });
  });

  it("replaces text generation selection across providers without leaking stale options", () => {
    const current = {
      ...DEFAULT_SERVER_SETTINGS,
      textGenerationModelSelection: createModelSelection(
        ProviderInstanceId.make("codex"),
        "gpt-5.4-mini",
        [
          { id: "reasoningEffort", value: "high" },
          { id: "fastMode", value: true },
        ],
      ),
    };

    expect(
      applyServerSettingsPatch(current, {
        textGenerationModelSelection: {
          instanceId: ProviderInstanceId.make("opencode"),
          model: "openai/gpt-5",
        },
      }).textGenerationModelSelection,
    ).toEqual({
      instanceId: "opencode",
      model: "openai/gpt-5",
    });
  });

  it("accepts array-based text generation selection patches", () => {
    expect(
      applyServerSettingsPatch(DEFAULT_SERVER_SETTINGS, {
        textGenerationModelSelection: {
          instanceId: ProviderInstanceId.make("opencode"),
          model: "openai/gpt-5",
          options: [
            { id: "variant", value: "prod" },
            { id: "agent", value: "build" },
          ],
        },
      }).textGenerationModelSelection,
    ).toEqual({
      instanceId: "opencode",
      model: "openai/gpt-5",
      options: [
        { id: "variant", value: "prod" },
        { id: "agent", value: "build" },
      ],
    });
  });

  it("replaces providerInstances maps so omitted instance fields are cleared", () => {
    const codexId = ProviderInstanceId.make("codex");
    const current = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: {
        [codexId]: {
          driver: ProviderDriverKind.make("codex"),
          displayName: "Codex Work",
          accentColor: "#7c3aed",
          enabled: true,
          config: { homePath: "~/.codex" },
        },
      },
    };

    expect(
      applyServerSettingsPatch(current, {
        providerInstances: {
          [codexId]: {
            driver: ProviderDriverKind.make("codex"),
            displayName: "Codex Work",
            enabled: true,
            config: { homePath: "~/.codex" },
          },
        },
      }).providerInstances[codexId],
    ).toEqual({
      driver: ProviderDriverKind.make("codex"),
      displayName: "Codex Work",
      enabled: true,
      config: { homePath: "~/.codex" },
    });
  });
});
