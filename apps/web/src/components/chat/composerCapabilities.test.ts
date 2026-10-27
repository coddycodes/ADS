import type { ServerProvider, ServerProviderSkill } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { deriveComposerCapabilityProfile } from "./composerCapabilities";

function makeSkill(
  name: string,
  invocation: ServerProviderSkill["invocation"],
  overrides: Partial<ServerProviderSkill> = {},
): ServerProviderSkill {
  return {
    name,
    path: `/skills/${name}`,
    enabled: true,
    invocation,
    ...overrides,
  };
}

function makeProviderInventory(
  input: Partial<Pick<ServerProvider, "slashCommands" | "skills" | "agents">> = {},
): Pick<ServerProvider, "slashCommands" | "skills" | "agents"> {
  return {
    slashCommands: [],
    skills: [],
    agents: [],
    ...input,
  };
}

describe("deriveComposerCapabilityProfile", () => {
  it("derives triggers from invocation metadata without driver branches", () => {
    const profile = deriveComposerCapabilityProfile({
      slashCommands: [{ name: "review" }],
      skills: [
        makeSkill("audit", "slash"),
        makeSkill("fix", "dollar"),
        makeSkill("explain", "prompt"),
      ],
      agents: [{ name: "reviewer", invocation: "mention" }, { name: "planner" }],
    });

    expect(profile.trigger).toEqual({
      providerSlash: true,
      providerDollarSkill: true,
    });
    expect(profile.signature).toBe("slash:dollar");
    expect(profile.slashCommands.map((command) => command.name)).toEqual(["review"]);
    expect(profile.slashSkills.map((skill) => skill.name)).toEqual(["audit"]);
    expect(profile.dollarSkills.map((skill) => skill.name)).toEqual(["fix"]);
    expect(profile.mentionableAgents.map((agent) => agent.name)).toEqual(["reviewer"]);
    expect([...profile.mentionableAgentNames]).toEqual(["reviewer"]);
  });

  it.each([
    {
      label: "Codex",
      provider: makeProviderInventory({
        skills: [makeSkill("review", "dollar")],
        agents: [{ name: "explorer", invocation: "mention" }],
      }),
      trigger: { providerSlash: false, providerDollarSkill: true },
      slashSkills: [],
      dollarSkills: ["review"],
      agents: ["explorer"],
    },
    {
      label: "Claude",
      provider: makeProviderInventory({
        slashCommands: [{ name: "compact" }],
        skills: [makeSkill("frontend-design", "slash")],
        agents: [{ name: "planner", invocation: "mention" }],
      }),
      trigger: { providerSlash: true, providerDollarSkill: false },
      slashSkills: ["frontend-design"],
      dollarSkills: [],
      agents: ["planner"],
    },
    {
      label: "Cursor",
      provider: makeProviderInventory({
        slashCommands: [{ name: "review" }],
      }),
      trigger: { providerSlash: true, providerDollarSkill: false },
      slashSkills: [],
      dollarSkills: [],
      agents: [],
    },
    {
      label: "OpenCode",
      provider: makeProviderInventory({
        skills: [makeSkill("audit", "slash"), makeSkill("fix", "dollar")],
      }),
      trigger: { providerSlash: true, providerDollarSkill: true },
      slashSkills: ["audit"],
      dollarSkills: ["fix"],
      agents: [],
    },
    {
      label: "Grok",
      provider: makeProviderInventory({
        skills: [makeSkill("explain", "prompt")],
        agents: [{ name: "planner" }],
      }),
      trigger: { providerSlash: false, providerDollarSkill: false },
      slashSkills: [],
      dollarSkills: [],
      agents: [],
    },
  ])(
    "derives the $label fixture only from its advertised inventory",
    ({ provider, trigger, slashSkills, dollarSkills, agents }) => {
      const profile = deriveComposerCapabilityProfile(provider);

      expect(profile.trigger).toEqual(trigger);
      expect(profile.slashSkills.map((skill) => skill.name)).toEqual(slashSkills);
      expect(profile.dollarSkills.map((skill) => skill.name)).toEqual(dollarSkills);
      expect(profile.mentionableAgents.map((agent) => agent.name)).toEqual(agents);
    },
  );

  it("returns an empty profile for missing or empty inventory", () => {
    for (const provider of [null, makeProviderInventory()]) {
      const profile = deriveComposerCapabilityProfile(provider);

      expect(profile.trigger).toEqual({
        providerSlash: false,
        providerDollarSkill: false,
      });
      expect(profile.signature).toBe(":");
      expect(profile.slashCommands).toEqual([]);
      expect(profile.slashSkills).toEqual([]);
      expect(profile.dollarSkills).toEqual([]);
      expect(profile.mentionableAgents).toEqual([]);
      expect([...profile.mentionableAgentNames]).toEqual([]);
    }
  });

  it("excludes disabled and prompt-only skills from inline menus", () => {
    const profile = deriveComposerCapabilityProfile(
      makeProviderInventory({
        skills: [
          makeSkill("disabled-slash", "slash", { enabled: false }),
          makeSkill("disabled-dollar", "dollar", { enabled: false }),
          makeSkill("prompt-only", "prompt"),
        ],
      }),
    );

    expect(profile.trigger).toEqual({
      providerSlash: false,
      providerDollarSkill: false,
    });
    expect(profile.slashSkills).toEqual([]);
    expect(profile.dollarSkills).toEqual([]);
  });

  it("lets commands win same-named slash skills while preserving inventory order", () => {
    const profile = deriveComposerCapabilityProfile(
      makeProviderInventory({
        slashCommands: [{ name: "review" }, { name: "help" }],
        skills: [
          makeSkill("audit", "slash"),
          makeSkill("Review", "slash"),
          makeSkill("design", "slash"),
          makeSkill("fix", "dollar"),
          makeSkill("format", "dollar"),
        ],
        agents: [
          { name: "reviewer", invocation: "mention" },
          { name: "planner" },
          { name: "explorer", invocation: "mention" },
        ],
      }),
    );

    expect(profile.slashCommands.map((command) => command.name)).toEqual(["review", "help"]);
    expect(profile.slashSkills.map((skill) => skill.name)).toEqual(["audit", "design"]);
    expect(profile.dollarSkills.map((skill) => skill.name)).toEqual(["fix", "format"]);
    expect(profile.mentionableAgents.map((agent) => agent.name)).toEqual(["reviewer", "explorer"]);
    expect([...profile.mentionableAgentNames]).toEqual(["reviewer", "explorer"]);
  });

  it("derives a fresh immutable result when provider inventory refreshes", () => {
    const initial = deriveComposerCapabilityProfile(makeProviderInventory());
    const refreshed = deriveComposerCapabilityProfile(
      makeProviderInventory({
        slashCommands: [{ name: "review" }],
        skills: [makeSkill("fix", "dollar")],
        agents: [{ name: "reviewer", invocation: "mention" }],
      }),
    );

    expect(initial.trigger).toEqual({
      providerSlash: false,
      providerDollarSkill: false,
    });
    expect([...initial.mentionableAgentNames]).toEqual([]);
    expect(refreshed.trigger).toEqual({
      providerSlash: true,
      providerDollarSkill: true,
    });
    expect([...refreshed.mentionableAgentNames]).toEqual(["reviewer"]);
  });
});

it("keeps native skill menus reachable while a catalog is empty or loading", () => {
  expect(deriveComposerCapabilityProfile(null, "dollar").trigger.providerDollarSkill).toBe(true);
  expect(
    deriveComposerCapabilityProfile(makeProviderInventory(), "slash").trigger.providerSlash,
  ).toBe(true);
  expect(deriveComposerCapabilityProfile(null, "slash").trigger.providerDollarSkill).toBe(false);
});
