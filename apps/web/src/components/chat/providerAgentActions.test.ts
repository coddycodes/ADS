import {
  DEFAULT_DEFAULT_AGENT_SELECTION,
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH,
  type ServerProvider,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildProviderAgentActions,
  resolveEffectiveProviderAgentAction,
} from "./providerAgentActions";

function provider(input: {
  instanceId: string;
  driver: string;
  displayName: string;
  status?: "ready" | "error";
}): ServerProvider {
  return {
    instanceId: ProviderInstanceId.make(input.instanceId),
    driver: input.driver,
    displayName: input.displayName,
    enabled: true,
    installed: true,
    status: input.status ?? "ready",
    availability: "available",
    models: [],
  } as unknown as ServerProvider;
}

const settings = {
  ...DEFAULT_SERVER_SETTINGS,
  providers: {
    ...DEFAULT_SERVER_SETTINGS.providers,
    opencode: { ...DEFAULT_SERVER_SETTINGS.providers.opencode, enabled: false },
  },
};

const readyCodex = provider({ instanceId: "codex", driver: "codex", displayName: "Codex" });
const readyClaude = provider({
  instanceId: "claudeAgent",
  driver: "claudeAgent",
  displayName: "Claude",
});
const disabledOpenCode = provider({
  instanceId: "opencode",
  driver: "opencode",
  displayName: "OpenCode",
});
const readyGrok = provider({ instanceId: "grok", driver: "grok", displayName: "Grok" });
const unreadyCodex = provider({
  instanceId: "codex",
  driver: "codex",
  displayName: "Codex",
  status: "error",
});

describe("buildProviderAgentActions", () => {
  it("builds chats first and supported AI terminals second", () => {
    const actions = buildProviderAgentActions(
      [readyCodex, readyClaude, disabledOpenCode],
      settings,
    );

    expect(actions.map(({ value }) => value)).toEqual([
      "chat:codex",
      "chat:claudeAgent",
      "terminal:codex",
      "terminal:claudeAgent",
    ]);
  });

  it("never exposes Grok chat or terminal actions even if legacy settings enable it", () => {
    const actions = buildProviderAgentActions([readyCodex, readyGrok], {
      ...settings,
      providers: {
        ...settings.providers,
        grok: { ...settings.providers.grok, enabled: true },
      },
    });

    expect(actions.map(({ value }) => value)).toEqual(["chat:codex", "terminal:codex"]);
  });

  it("keeps visible but unready providers disabled", () => {
    const [chat, terminal] = buildProviderAgentActions([unreadyCodex], settings);

    expect(chat).toMatchObject({ value: "chat:codex", disabled: true });
    expect(terminal).toMatchObject({ value: "terminal:codex", disabled: true });
  });

  it("keeps unsupported terminal commands visible but disabled with their reason", () => {
    const actions = buildProviderAgentActions([readyCodex], {
      ...settings,
      providerInstances: {
        [ProviderInstanceId.make("codex")]: {
          driver: ProviderDriverKind.make("codex"),
          config: { binaryPath: "x".repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH + 1) },
        },
      },
    });

    expect(actions.find((action) => action.value === "terminal:codex")).toMatchObject({
      disabled: true,
      disabledReason:
        "Provider terminal command exceeds supported limits. Shorten the provider name or configured binary path.",
      terminalAction: {
        command: null,
        disabledReason:
          "Provider terminal command exceeds supported limits. Shorten the provider name or configured binary path.",
      },
    });
  });

  it("falls back to the first selectable chat without rewriting the saved selection", () => {
    const actions = buildProviderAgentActions([readyClaude, readyCodex], settings);

    expect(
      resolveEffectiveProviderAgentAction(actions, {
        kind: "terminal",
        instanceId: ProviderInstanceId.make("missing"),
      })?.value,
    ).toBe("chat:claudeAgent");
  });

  it("returns null when no chat provider is picker-ready", () => {
    const actions = buildProviderAgentActions([unreadyCodex], settings);

    expect(
      resolveEffectiveProviderAgentAction(actions, DEFAULT_DEFAULT_AGENT_SELECTION),
    ).toBeNull();
  });
});
