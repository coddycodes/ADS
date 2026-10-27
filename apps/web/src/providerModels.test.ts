import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProvider,
  type ServerProviderModel,
} from "@coddy/contracts";
import { createModelCapabilities } from "@coddy/shared/model";
import { describe, expect, it } from "vite-plus/test";

import { getProviderInteractionModeToggle, getProviderModelCapabilities } from "./providerModels";

const codex = ProviderDriverKind.make("codex");

function provider(instanceId: string): ServerProvider {
  return {
    instanceId: ProviderInstanceId.make(instanceId),
    driver: codex,
    enabled: true,
    installed: true,
    version: "1.0.0",
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-08-03T00:00:00.000Z",
    models: [],
    slashCommands: [],
    skills: [],
    agents: [],
  };
}

describe("getProviderModelCapabilities", () => {
  it("uses a directly selected dynamic provider model before alias normalization", () => {
    const capabilities = createModelCapabilities({
      optionDescriptors: [{ id: "fastMode", label: "Fast Mode", type: "boolean" }],
    });
    const models: ReadonlyArray<ServerProviderModel> = [
      { slug: "opus", name: "Opus", isCustom: false, capabilities },
    ];

    expect(
      getProviderModelCapabilities(models, "opus", ProviderDriverKind.make("claudeAgent")),
    ).toBe(capabilities);
  });
});

describe("getProviderInteractionModeToggle", () => {
  it("does not borrow the default instance state for a missing explicit instance", () => {
    expect(
      getProviderInteractionModeToggle(
        [provider("codex")],
        codex,
        ProviderInstanceId.make("codex-work"),
        true,
      ),
    ).toEqual({ state: "unknown", reason: "Plan mode availability is still loading." });
  });
});
