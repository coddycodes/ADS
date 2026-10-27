import {
  DEFAULT_MODEL_BY_PROVIDER,
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT,
  TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH,
  TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH,
  TERMINAL_LAUNCH_LABEL_MAX_LENGTH,
  type ProviderOptionDescriptor,
  type ServerSettings,
  type ServerProviderModel,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import type { ProviderInstanceEntry } from "~/providerInstances";
import { decodeTerminalLaunchCommand } from "~/lib/terminalLaunchCommand";
import { resolveProviderTerminalAction } from "./providerTerminalActions";

const CODEX_DRIVER = ProviderDriverKind.make("codex");
const CLAUDE_DRIVER = ProviderDriverKind.make("claudeAgent");
const CURSOR_DRIVER = ProviderDriverKind.make("cursor");
const GROK_DRIVER = ProviderDriverKind.make("grok");
const OPENCODE_DRIVER = ProviderDriverKind.make("opencode");

function entry(
  driver: string,
  instanceId = driver,
  displayName = driver,
  models: ReadonlyArray<ServerProviderModel> = [],
): ProviderInstanceEntry {
  return {
    instanceId: ProviderInstanceId.make(instanceId),
    driverKind: ProviderDriverKind.make(driver),
    displayName,
    accentColor: undefined,
    continuationGroupKey: undefined,
    enabled: true,
    installed: true,
    status: "ready",
    isDefault: instanceId === driver,
    isAvailable: true,
    snapshot: {} as ProviderInstanceEntry["snapshot"],
    models,
  };
}

function model(
  slug: string,
  optionDescriptors: ReadonlyArray<ProviderOptionDescriptor>,
): ServerProviderModel {
  return {
    slug,
    name: slug,
    isCustom: false,
    capabilities: { optionDescriptors: [...optionDescriptors] },
  };
}

const effortDescriptor: ProviderOptionDescriptor = {
  id: "reasoningEffort",
  label: "Reasoning",
  type: "select",
  options: [
    { id: "medium", label: "Medium", isDefault: true },
    { id: "high", label: "High" },
  ],
  currentValue: "medium",
};

const claudeEffortDescriptor: ProviderOptionDescriptor = {
  ...effortDescriptor,
  id: "effort",
};

const claudePromptInjectedEffortDescriptor: ProviderOptionDescriptor = {
  ...claudeEffortDescriptor,
  options: [
    { id: "high", label: "High", isDefault: true },
    { id: "ultrathink", label: "Ultrathink" },
  ],
  promptInjectedValues: ["ultrathink"],
};

const cursorEffortDescriptor: ProviderOptionDescriptor = {
  ...effortDescriptor,
  id: "reasoning",
};

const fastModeDescriptor: ProviderOptionDescriptor = {
  id: "fastMode",
  label: "Fast mode",
  type: "boolean",
  currentValue: false,
};

const serviceTierDescriptor: ProviderOptionDescriptor = {
  id: "serviceTier",
  label: "Service tier",
  type: "select",
  options: [
    { id: "default", label: "Default", isDefault: true },
    { id: "fast", label: "Fast" },
  ],
  currentValue: "default",
};

const partialServiceTierDescriptor: ProviderOptionDescriptor = {
  id: "serviceTier",
  label: "Live service tier",
  type: "select",
  options: [{ id: "default", label: "Live Standard", isDefault: true }],
  currentValue: "default",
};

describe("resolveProviderTerminalAction", () => {
  it.each([
    {
      name: "Codex",
      driverKind: CODEX_DRIVER,
      models: [model("gpt-5.4", [effortDescriptor])],
      configuredDefault: {
        model: "gpt-5.4",
        options: [
          { id: "reasoningEffort", value: "high" },
          { id: "serviceTier", value: "fast" },
        ],
      },
      expected: {
        executable: "codex",
        args: [
          "--dangerously-bypass-approvals-and-sandbox",
          "--model",
          "gpt-5.4",
          "--config",
          'model_reasoning_effort="high"',
        ],
        label: "Codex Terminal",
        activity: {
          driverKind: CODEX_DRIVER,
          providerInstanceId: ProviderInstanceId.make("codex"),
        },
      },
    },
    {
      name: "Claude",
      driverKind: CLAUDE_DRIVER,
      models: [model("claude-sonnet-5", [claudeEffortDescriptor])],
      configuredDefault: {
        model: "claude-sonnet-5",
        options: [{ id: "effort", value: "high" }],
      },
      expected: {
        executable: "claude",
        args: ["--dangerously-skip-permissions", "--model", "claude-sonnet-5", "--effort", "high"],
        label: "Claude Terminal",
        activity: {
          driverKind: CLAUDE_DRIVER,
          providerInstanceId: ProviderInstanceId.make("claudeAgent"),
        },
      },
    },
    {
      name: "Cursor",
      driverKind: CURSOR_DRIVER,
      models: [model("cursor-large[legacy=true]", [cursorEffortDescriptor, fastModeDescriptor])],
      configuredDefault: {
        model: "cursor-large[legacy=true]",
        options: [
          { id: "reasoning", value: "high" },
          { id: "fastMode", value: true },
        ],
      },
      expected: {
        executable: "cursor-agent",
        args: ["--yolo", "--model", "cursor-large[effort=high,fast=true]"],
        label: "Cursor Terminal",
      },
    },
    {
      name: "Grok",
      driverKind: GROK_DRIVER,
      models: [model("grok-4", [claudeEffortDescriptor])],
      configuredDefault: {
        model: "grok-4",
        options: [{ id: "effort", value: "high" }],
      },
      expected: {
        executable: "grok",
        args: ["--permission-mode", "bypassPermissions", "--model", "grok-4", "--effort", "high"],
        label: "Grok Terminal",
      },
    },
    {
      name: "OpenCode",
      driverKind: OPENCODE_DRIVER,
      models: [model("openai/gpt-5", [claudeEffortDescriptor, fastModeDescriptor])],
      configuredDefault: {
        model: "openai/gpt-5",
        options: [
          { id: "effort", value: "high" },
          { id: "fastMode", value: true },
        ],
      },
      expected: {
        executable: "opencode",
        args: ["--model", "openai/gpt-5"],
        env: { OPENCODE_CONFIG_CONTENT: '{"theme":"system"}' },
        label: "OpenCode Terminal",
        activity: {
          driverKind: OPENCODE_DRIVER,
          providerInstanceId: ProviderInstanceId.make("opencode"),
        },
      },
    },
  ])(
    "resolves $name with the exact supported executable and argument vector",
    ({ name, driverKind, models, configuredDefault, expected }) => {
      const settings = {
        ...DEFAULT_SERVER_SETTINGS,
        providerSessionDefaults: {
          [driverKind]: configuredDefault,
        },
      } satisfies ServerSettings;

      expect(
        resolveProviderTerminalAction(entry(driverKind, driverKind, name, models), settings)
          ?.command,
      ).toEqual(expected);
    },
  );

  it("omits unverified Codex Fast arguments during empty discovery", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CODEX_DRIVER]: {
          model: "gpt-offline",
          options: [
            { id: "reasoningEffort", value: "xhigh" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(entry("codex", "codex", "Codex", []), settings)?.command?.args,
    ).toEqual([
      "--dangerously-bypass-approvals-and-sandbox",
      "--model",
      DEFAULT_MODEL_BY_PROVIDER[CODEX_DRIVER],
      "--config",
      'model_reasoning_effort="xhigh"',
    ]);
  });

  it("omits Codex Fast arguments through a partial live serviceTier descriptor", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CODEX_DRIVER]: {
          model: "gpt-5.6-sol",
          options: [{ id: "serviceTier", value: "fast" }],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("codex", "codex", "Codex", [
          model("gpt-5.6-sol", [effortDescriptor, partialServiceTierDescriptor]),
        ]),
        settings,
      )?.command?.args,
    ).toEqual([
      "--dangerously-bypass-approvals-and-sandbox",
      "--model",
      "gpt-5.6-sol",
      "--config",
      'model_reasoning_effort="medium"',
    ]);
  });

  it("builds Claude model and effort arguments and omits unsupported fast mode", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CLAUDE_DRIVER]: {
          model: "claude-opus-4-8",
          options: [
            { id: "effort", value: "high" },
            { id: "fastMode", value: true },
          ],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("claudeAgent", "claudeAgent", "Claude", [
          model("claude-opus-4-8", [claudeEffortDescriptor, fastModeDescriptor]),
        ]),
        settings,
      )?.command?.args,
    ).toEqual(["--dangerously-skip-permissions", "--model", "claude-opus-4-8", "--effort", "high"]);
  });

  it.each([
    {
      effort: "ultrathink",
      expected: ["--dangerously-skip-permissions", "--model", "claude-opus-4-8"],
    },
    {
      effort: "high",
      expected: [
        "--dangerously-skip-permissions",
        "--model",
        "claude-opus-4-8",
        "--effort",
        "high",
      ],
    },
  ])("maps the Claude $effort default to supported terminal arguments", ({ effort, expected }) => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CLAUDE_DRIVER]: {
          model: "claude-opus-4-8",
          options: [{ id: "effort", value: effort }],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("claudeAgent", "claudeAgent", "Claude", [
          model("claude-opus-4-8", [claudePromptInjectedEffortDescriptor]),
        ]),
        settings,
      )?.command?.args,
    ).toEqual(expected);
  });

  it.each([
    {
      effort: "ultrathink",
      expected: [
        "--dangerously-skip-permissions",
        "--model",
        DEFAULT_MODEL_BY_PROVIDER[CLAUDE_DRIVER],
      ],
    },
    {
      effort: "high",
      expected: [
        "--dangerously-skip-permissions",
        "--model",
        DEFAULT_MODEL_BY_PROVIDER[CLAUDE_DRIVER],
        "--effort",
        "high",
      ],
    },
  ])(
    "conservatively maps the Claude $effort default during empty discovery",
    ({ effort, expected }) => {
      const settings = {
        ...DEFAULT_SERVER_SETTINGS,
        providerSessionDefaults: {
          [CLAUDE_DRIVER]: {
            model: "claude-offline",
            options: [{ id: "effort", value: effort }],
          },
        },
      } satisfies ServerSettings;

      expect(
        resolveProviderTerminalAction(entry("claudeAgent", "claudeAgent", "Claude", []), settings)
          ?.command?.args,
      ).toEqual(expected);
    },
  );

  it('passes Codex fast mode off as an explicit service_tier="default" config', () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CODEX_DRIVER]: {
          model: "gpt-5.4",
          options: [{ id: "serviceTier", value: "default" }],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("codex", "codex", "Codex", [model("gpt-5.4", [serviceTierDescriptor])]),
        settings,
      )?.command?.args,
    ).toEqual([
      "--dangerously-bypass-approvals-and-sandbox",
      "--model",
      "gpt-5.4",
      "--config",
      'service_tier="default"',
    ]);
  });

  it("omits Claude fast mode from terminal arguments", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CLAUDE_DRIVER]: {
          model: "claude-sonnet-5",
          options: [{ id: "fastMode", value: true }],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("claudeAgent", "claudeAgent", "Claude", [
          model("claude-sonnet-5", [fastModeDescriptor]),
        ]),
        settings,
      )?.command?.args,
    ).toEqual(["--dangerously-skip-permissions", "--model", "claude-sonnet-5"]);
  });

  it("omits Grok fast mode from terminal arguments", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [GROK_DRIVER]: {
          model: "grok-4",
          options: [{ id: "fastMode", value: true }],
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(
        entry("grok", "grok", "Grok", [model("grok-4", [fastModeDescriptor])]),
        settings,
      )?.command?.args,
    ).toEqual(["--permission-mode", "bypassPermissions", "--model", "grok-4"]);
  });

  it("preserves the verified Codex effort after a model fallback", () => {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [CODEX_DRIVER]: {
          model: "retired-codex-model",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    } satisfies ServerSettings;

    const action = resolveProviderTerminalAction(
      entry("codex", "codex", "Codex", [model("gpt-5.4-mini", [])]),
      settings,
    );
    expect(action?.command?.args).toEqual([
      "--dangerously-bypass-approvals-and-sandbox",
      "--model",
      "gpt-5.4-mini",
      "--config",
      'model_reasoning_effort="high"',
    ]);
    expect(action).toMatchObject({
      fallback: {
        driver: "codex",
        instanceId: "codex",
        configuredModel: "retired-codex-model",
        resolvedModel: "gpt-5.4-mini",
        reason: "configured-model-unavailable",
      },
    });
  });

  it("launches opencode with the system theme so the TUI follows the app theme", () => {
    const action = resolveProviderTerminalAction(
      entry("opencode", "opencode", "OpenCode"),
      DEFAULT_SERVER_SETTINGS,
    );
    expect(action?.command).toMatchObject({
      executable: "opencode",
      env: { OPENCODE_CONFIG_CONTENT: '{"theme":"system"}' },
    });
  });

  it("leaves the launch environment unset for providers without env defaults", () => {
    const action = resolveProviderTerminalAction(
      entry("claudeAgent", "claudeAgent", "Claude"),
      DEFAULT_SERVER_SETTINGS,
    );
    expect(action?.command?.env).toBe(undefined);
  });

  it("prefers instance paths, then legacy paths, and preserves custom names", () => {
    const customId = ProviderInstanceId.make("codex_personal");
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providers: {
        ...DEFAULT_SERVER_SETTINGS.providers,
        codex: {
          ...DEFAULT_SERVER_SETTINGS.providers.codex,
          binaryPath: "/legacy/codex",
        },
      },
      providerInstances: {
        [customId]: {
          driver: ProviderDriverKind.make("codex"),
          displayName: "Personal Codex",
          config: { binaryPath: " /custom/Codex CLI " },
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(entry("codex", customId, "Personal Codex"), settings),
    ).toMatchObject({
      label: "Personal Codex Terminal",
      command: {
        executable: "/custom/Codex CLI",
        activity: {
          driverKind: CODEX_DRIVER,
          providerInstanceId: customId,
        },
      },
    });

    const inherited = {
      ...settings,
      providerInstances: {
        [customId]: {
          ...settings.providerInstances[customId]!,
          config: {},
        },
      },
    };
    expect(
      resolveProviderTerminalAction(entry("codex", customId, "Personal Codex"), inherited),
    ).toMatchObject({
      command: {
        executable: "/legacy/codex",
        activity: {
          driverKind: CODEX_DRIVER,
          providerInstanceId: customId,
        },
      },
    });
  });

  it("falls through whitespace-only instance and legacy binary paths", () => {
    const customId = ProviderInstanceId.make("codex_personal");
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providers: {
        ...DEFAULT_SERVER_SETTINGS.providers,
        codex: {
          ...DEFAULT_SERVER_SETTINGS.providers.codex,
          binaryPath: " /legacy/codex ",
        },
      },
      providerInstances: {
        [customId]: {
          driver: ProviderDriverKind.make("codex"),
          config: { binaryPath: " \t " },
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(entry("codex", customId, "Personal Codex"), settings),
    ).toMatchObject({ command: { executable: "/legacy/codex" } });

    expect(
      resolveProviderTerminalAction(entry("codex", customId, "Personal Codex"), {
        ...settings,
        providers: {
          ...settings.providers,
          codex: {
            ...settings.providers.codex,
            binaryPath: " \n ",
          },
        },
      }),
    ).toMatchObject({ command: { executable: "codex" } });
  });

  it("preserves an explicit non-default Cursor binary path", () => {
    const customId = ProviderInstanceId.make("cursor_work");
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: {
        [customId]: {
          driver: ProviderDriverKind.make("cursor"),
          config: { binaryPath: " /Applications/Cursor Agent/custom-agent " },
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(entry("cursor", customId, "Work Cursor"), settings),
    ).toMatchObject({
      command: { executable: "/Applications/Cursor Agent/custom-agent" },
    });
  });

  it("does not invent command semantics for an unregistered driver", () => {
    const action = resolveProviderTerminalAction(
      entry("forkDriver", "fork", "Fork"),
      DEFAULT_SERVER_SETTINGS,
    );
    expect(action).toBeNull();
    expect(
      decodeTerminalLaunchCommand({
        executable: "fork-cli",
        args: [],
        label: "Fork shell",
      }),
    ).not.toHaveProperty("activity");
  });

  it.each([
    { driver: CURSOR_DRIVER, executable: "cursor-agent" },
    { driver: GROK_DRIVER, executable: "grok" },
  ])("never attaches observer activity to $driver terminal commands", ({ driver, executable }) => {
    const action = resolveProviderTerminalAction(entry(driver, driver), DEFAULT_SERVER_SETTINGS);

    expect(action?.command?.executable).toBe(executable);
    expect(action?.command).not.toHaveProperty("activity");
  });

  it.each([
    {
      name: "configured executable",
      displayName: "Codex",
      binaryPath: "x".repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH + 1),
    },
    {
      name: "display label",
      displayName: "x".repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH),
      binaryPath: "codex",
    },
  ])("disables an action with an oversized $name", ({ displayName, binaryPath }) => {
    const instanceId = ProviderInstanceId.make("codex_bounded");
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: {
        [instanceId]: {
          driver: ProviderDriverKind.make("codex"),
          displayName,
          config: { binaryPath },
        },
      },
    } satisfies ServerSettings;

    expect(
      resolveProviderTerminalAction(entry("codex", instanceId, displayName), settings),
    ).toEqual({
      entry: entry("codex", instanceId, displayName),
      label: `${displayName} Terminal`,
      command: null,
      disabledReason:
        "Provider terminal command exceeds supported limits. Shorten the provider name or configured binary path.",
    });
  });

  it("disables an action when a resolved model argument exceeds command bounds", () => {
    const oversizedModel = "x".repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH + 1);
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerSessionDefaults: {
        [OPENCODE_DRIVER]: { model: oversizedModel },
      },
    } satisfies ServerSettings;
    const providerEntry = entry("opencode", "opencode", "OpenCode", [model(oversizedModel, [])]);

    expect(resolveProviderTerminalAction(providerEntry, settings)).toEqual({
      entry: providerEntry,
      label: "OpenCode Terminal",
      command: null,
      disabledReason:
        "Provider terminal command exceeds supported limits. Shorten the provider name or configured binary path.",
    });
  });

  it("decodes executable, argument, count, and label boundaries before launch", () => {
    const atBoundary = {
      executable: "x".repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH),
      args: Array.from({ length: TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT }, () =>
        "x".repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH),
      ),
      label: "x".repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH),
    };
    expect(decodeTerminalLaunchCommand(atBoundary)).toEqual(atBoundary);
    expect(
      decodeTerminalLaunchCommand({ executable: "/bin/zsh", args: ["-l"] }),
    ).not.toHaveProperty("activity");

    for (const candidate of [
      { ...atBoundary, executable: `${atBoundary.executable}x` },
      { ...atBoundary, args: [`${atBoundary.args[0]}x`] },
      { ...atBoundary, args: [...atBoundary.args, "x"] },
      { ...atBoundary, label: `${atBoundary.label}x` },
    ]) {
      expect(decodeTerminalLaunchCommand(candidate)).toBeNull();
    }
  });
});
