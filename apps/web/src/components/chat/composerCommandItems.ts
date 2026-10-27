import type {
  ProjectEntry,
  ProviderInstanceId,
  ServerProviderAgent,
  ServerProviderSkill,
  ServerProviderSlashCommand,
} from "@coddy/contracts";
import { serializeComposerReference } from "@coddy/shared/composerReferences";
import type { ComposerCoddyStudioAction, ComposerTrigger } from "@coddy/shared/composerTrigger";

import { searchProviderSkills } from "../../providerSkillSearch";
import type { ComposerCapabilityProfile } from "./composerCapabilities";
import { searchComposerCommandCandidates } from "./composerCommandSearch";

export type ComposerCommandGroupId = "coddy-studio" | "commands" | "skills" | "files" | "agents";

interface ComposerCommandItemBase {
  readonly id: string;
  readonly group: ComposerCommandGroupId;
  readonly label: string;
  readonly description: string;
}

export interface CoddyStudioActionItem extends ComposerCommandItemBase {
  readonly type: "coddy-studio-action";
  readonly group: "coddy-studio";
  readonly action: ComposerCoddyStudioAction;
  readonly replacement: null;
}

export interface ProviderCommandItem extends ComposerCommandItemBase {
  readonly type: "provider-command";
  readonly group: "commands";
  readonly providerInstanceId: ProviderInstanceId;
  readonly command: ServerProviderSlashCommand;
  readonly replacement: string;
}

export interface ProviderSkillItem extends ComposerCommandItemBase {
  readonly type: "provider-skill";
  readonly group: "skills";
  readonly providerInstanceId: ProviderInstanceId;
  readonly skill: ServerProviderSkill;
  readonly replacement: string;
}

export interface FileReferenceItem extends ComposerCommandItemBase {
  readonly type: "file-reference";
  readonly group: "files";
  readonly path: string;
  readonly pathKind: ProjectEntry["kind"];
  readonly replacement: string;
}

export interface AgentReferenceItem extends ComposerCommandItemBase {
  readonly type: "agent-reference";
  readonly group: "agents";
  readonly providerInstanceId: ProviderInstanceId;
  readonly agent: ServerProviderAgent;
  readonly replacement: string;
}

export type ComposerCommandItem =
  | CoddyStudioActionItem
  | ProviderCommandItem
  | ProviderSkillItem
  | FileReferenceItem
  | AgentReferenceItem;

export interface ComposerCommandItemsInput {
  readonly trigger: ComposerTrigger | null;
  readonly providerInstanceId: ProviderInstanceId;
  readonly capabilities: ComposerCapabilityProfile;
  readonly pathSearch: {
    readonly entries: ReadonlyArray<ProjectEntry>;
    readonly error: unknown | null;
    readonly isPending: boolean;
  };
}

export interface ComposerCommandItemsResult {
  readonly items: ReadonlyArray<ComposerCommandItem>;
  readonly preferredItemId: string | null;
  readonly emptyStateText: string;
}

const CODDY_STUDIO_ACTIONS: ReadonlyArray<{
  readonly action: ComposerCoddyStudioAction;
  readonly description: string;
}> = [
  { action: "model", description: "Switch response model for this thread" },
  { action: "plan", description: "Switch this thread into plan mode" },
  { action: "default", description: "Switch this thread back to normal build mode" },
];

function normalizedQuery(query: string): string {
  return query.trim().toLowerCase();
}

function basenameOfPath(path: string): string {
  const separatorIndex = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separatorIndex >= 0 ? path.slice(separatorIndex + 1) : path;
}

function parentOfPath(path: string): string {
  const separatorIndex = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separatorIndex >= 0 ? path.slice(0, separatorIndex) : "";
}

function providerItemId(
  type: "provider-command" | "provider-skill" | "agent-reference",
  providerInstanceId: ProviderInstanceId,
  name: string,
): string {
  return `${type}:${providerInstanceId}:${name}`;
}

function compareText(left: string, right: string): number {
  return left.toLowerCase().localeCompare(right.toLowerCase()) || left.localeCompare(right);
}

function compareSkills(left: ServerProviderSkill, right: ServerProviderSkill): number {
  return (
    compareText(left.name, right.name) ||
    compareText(left.path, right.path) ||
    compareText(left.description ?? "", right.description ?? "")
  );
}

function buildCoddyStudioActionItems(query: string): CoddyStudioActionItem[] {
  const normalized = normalizedQuery(query);
  return CODDY_STUDIO_ACTIONS.filter(({ action }) => !normalized || action.includes(normalized)).map(
    ({ action, description }) => ({
      id: `coddy-studio-action:${action}`,
      type: "coddy-studio-action",
      group: "coddy-studio",
      action,
      label: `:${action}`,
      description,
      replacement: null,
    }),
  );
}

function buildSlashItems(
  input: ComposerCommandItemsInput,
  query: string,
): Array<ProviderCommandItem | ProviderSkillItem> {
  if (!input.capabilities.trigger.providerSlash) {
    return [];
  }

  const seenNames = new Set<string>();
  const commands: ProviderCommandItem[] = [];
  const orderedCommands = [...input.capabilities.slashCommands].sort(
    (left, right) =>
      compareText(left.name, right.name) ||
      compareText(left.description ?? "", right.description ?? ""),
  );
  for (const command of orderedCommands) {
    const name = command.name.trim();
    const comparableName = name.toLowerCase();
    if (seenNames.has(comparableName)) {
      continue;
    }
    seenNames.add(comparableName);
    commands.push({
      id: providerItemId("provider-command", input.providerInstanceId, name),
      type: "provider-command",
      group: "commands",
      providerInstanceId: input.providerInstanceId,
      command,
      label: `/${name}`,
      description: command.description ?? command.input?.hint ?? "Run provider command",
      replacement: `/${name} `,
    });
  }

  const rankedCommands = searchComposerCommandCandidates(
    commands.map((item) => ({
      item,
      name: item.command.name,
      description: item.description,
      tieBreaker: item.id,
    })),
    query,
    { trimLeadingPattern: /^\/+/ },
  );
  const skills = searchProviderSkills(
    [...input.capabilities.slashSkills].sort(compareSkills),
    query,
  )
    .filter((skill) => {
      const comparableName = skill.name.trim().toLowerCase();
      if (seenNames.has(comparableName)) {
        return false;
      }
      seenNames.add(comparableName);
      return true;
    })
    .map((skill): ProviderSkillItem => ({
      id: providerItemId("provider-skill", input.providerInstanceId, `slash:${skill.name}`),
      type: "provider-skill",
      group: "skills",
      providerInstanceId: input.providerInstanceId,
      skill,
      label: `/${skill.name}`,
      description:
        skill.shortDescription ??
        skill.description ??
        (skill.scope ? `${skill.scope} skill` : "Run provider skill"),
      replacement: `/${skill.name} `,
    }));

  return [...rankedCommands, ...skills];
}

function buildDollarSkillItems(
  input: ComposerCommandItemsInput,
  query: string,
): ProviderSkillItem[] {
  if (!input.capabilities.trigger.providerDollarSkill) {
    return [];
  }

  return searchProviderSkills([...input.capabilities.dollarSkills].sort(compareSkills), query).map(
    (skill) => ({
      id: providerItemId("provider-skill", input.providerInstanceId, `dollar:${skill.name}`),
      type: "provider-skill",
      group: "skills",
      providerInstanceId: input.providerInstanceId,
      skill,
      label: `$${skill.name}`,
      description:
        skill.shortDescription ??
        skill.description ??
        (skill.scope ? `${skill.scope} skill` : "Run provider skill"),
      replacement: `$${skill.name} `,
    }),
  );
}

function buildReferenceItems(
  input: ComposerCommandItemsInput,
  query: string,
): {
  readonly items: Array<FileReferenceItem | AgentReferenceItem>;
  readonly preferredItemId: string | null;
} {
  const files = [...input.pathSearch.entries]
    .sort((left, right) => compareText(left.path, right.path) || compareText(left.kind, right.kind))
    .map((entry): FileReferenceItem => ({
      id: `file-reference:${entry.kind}:${entry.path}`,
      type: "file-reference",
      group: "files",
      path: entry.path,
      pathKind: entry.kind,
      label: basenameOfPath(entry.path),
      description: parentOfPath(entry.path),
      replacement: `${serializeComposerReference(entry.path)} `,
    }));

  const normalized = normalizedQuery(query);
  const seenNames = new Set<string>();
  const agentItems = [...input.capabilities.mentionableAgents]
    .sort(
      (left, right) =>
        compareText(left.name, right.name) ||
        compareText(left.description ?? "", right.description ?? "") ||
        compareText(left.model ?? "", right.model ?? ""),
    )
    .filter((agent) => {
      const comparableName = agent.name.trim().toLowerCase();
      if (seenNames.has(comparableName)) {
        return false;
      }
      seenNames.add(comparableName);
      return true;
    })
    .map((agent): AgentReferenceItem => ({
      id: providerItemId("agent-reference", input.providerInstanceId, agent.name),
      type: "agent-reference",
      group: "agents",
      providerInstanceId: input.providerInstanceId,
      agent,
      label: `@${agent.name}`,
      description: agent.description ?? agent.model ?? "Use provider agent",
      replacement: `@${agent.name} `,
    }));
  const agents = searchComposerCommandCandidates(
    agentItems.map((item) => ({
      item,
      name: item.agent.name,
      description: item.description,
      tieBreaker: item.id,
    })),
    query,
  );

  const preferredItemId =
    agents.find((item) => item.agent.name.trim().toLowerCase() === normalized)?.id ?? null;
  return { items: [...files, ...agents], preferredItemId };
}

export function buildComposerCommandItems(
  input: ComposerCommandItemsInput,
): ComposerCommandItemsResult {
  if (!input.trigger) {
    return {
      items: [],
      preferredItemId: null,
      emptyStateText: "No matching command.",
    };
  }

  if (input.trigger.kind === "coddy-studio-action") {
    return {
      items: buildCoddyStudioActionItems(input.trigger.query),
      preferredItemId: null,
      emptyStateText: "No matching Coddy Studio action.",
    };
  }
  if (input.trigger.kind === "provider-slash") {
    return {
      items: buildSlashItems(input, input.trigger.query),
      preferredItemId: null,
      emptyStateText: "No matching provider command or skill.",
    };
  }
  if (input.trigger.kind === "provider-dollar-skill") {
    return {
      items: buildDollarSkillItems(input, input.trigger.query),
      preferredItemId: null,
      emptyStateText: "No matching provider skill.",
    };
  }

  const references = buildReferenceItems(input, input.trigger.query);
  return {
    ...references,
    emptyStateText: input.pathSearch.error
      ? "File search failed. Agent results may still be available."
      : "No matching files or agents.",
  };
}
