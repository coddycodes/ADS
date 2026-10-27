import type {
  ServerProvider,
  ServerProviderAgent,
  ServerProviderSkill,
  ServerProviderSlashCommand,
} from "@coddy/contracts";
import type { ComposerTriggerProfile } from "@coddy/shared/composerTrigger";

export interface ComposerCapabilityProfile {
  readonly signature: string;
  readonly trigger: ComposerTriggerProfile;
  readonly slashCommands: ReadonlyArray<ServerProviderSlashCommand>;
  readonly slashSkills: ReadonlyArray<ServerProviderSkill>;
  readonly dollarSkills: ReadonlyArray<ServerProviderSkill>;
  readonly mentionableAgents: ReadonlyArray<ServerProviderAgent>;
  readonly mentionableAgentNames: ReadonlySet<string>;
}

export function deriveComposerCapabilityProfile(
  provider: Pick<ServerProvider, "slashCommands" | "skills" | "agents"> | null,
  nativeSkillInvocation?: "dollar" | "slash",
): ComposerCapabilityProfile {
  const slashCommands = provider?.slashCommands ?? [];
  const commandNames = new Set(slashCommands.map((command) => command.name.toLowerCase()));
  const slashSkills: ServerProviderSkill[] = [];
  const dollarSkills: ServerProviderSkill[] = [];

  for (const skill of provider?.skills ?? []) {
    if (!skill.enabled) {
      continue;
    }
    if (skill.invocation === "slash" && !commandNames.has(skill.name.toLowerCase())) {
      slashSkills.push(skill);
      continue;
    }
    if (skill.invocation === "dollar") {
      dollarSkills.push(skill);
    }
  }

  const mentionableAgents = (provider?.agents ?? []).filter(
    (agent) => agent.invocation === "mention",
  );
  const providerSlash =
    slashCommands.length > 0 || slashSkills.length > 0 || nativeSkillInvocation === "slash";
  const providerDollarSkill = dollarSkills.length > 0 || nativeSkillInvocation === "dollar";

  return {
    signature: `${providerSlash ? "slash" : ""}:${providerDollarSkill ? "dollar" : ""}`,
    trigger: {
      providerSlash,
      providerDollarSkill,
    },
    slashCommands,
    slashSkills,
    dollarSkills,
    mentionableAgents,
    mentionableAgentNames: new Set(mentionableAgents.map((agent) => agent.name)),
  };
}
