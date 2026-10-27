import type { EnvironmentId, SidebarRepositorySortOrder } from "@coddy/contracts";

import {
  compareSidebarDisplayText,
  type SidebarProjectSnapshot,
} from "../../sidebarProjectGrouping";
import {
  environmentLetterAvatar,
  isLocalRailCandidate,
  resolveEnvironmentRailStatus,
  type EnvironmentRailCandidate,
  type EnvironmentRailStatus,
} from "./environmentRail.logic";

export interface EnvironmentCardIdentity {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  /** This device's own environment; sorts before desktop-local ones. */
  readonly isPrimary: boolean;
  readonly isLocal: boolean;
  readonly avatar: string;
  readonly status: EnvironmentRailStatus;
  readonly statusLabel: string;
  readonly available: boolean;
}

export interface RepositoryGroup {
  readonly key: string;
  readonly title: string;
  readonly host: string | null;
  readonly showHost: boolean;
  readonly environmentCount: number;
  readonly cards: readonly SidebarProjectSnapshot[];
}

function statusLabel(candidate: EnvironmentRailCandidate): string {
  switch (candidate.phase) {
    case "connected":
      return candidate.isPrimary ? "This device" : "Connected";
    case "connecting":
    case "reconnecting":
      return "Reconnecting";
    case "error":
      return "Connection error";
    default:
      return "Offline";
  }
}

export function buildEnvironmentCardIdentities(
  candidates: readonly EnvironmentRailCandidate[],
): ReadonlyMap<EnvironmentId, EnvironmentCardIdentity> {
  return new Map(
    candidates.map((candidate) => [
      candidate.environmentId,
      {
        environmentId: candidate.environmentId,
        label: candidate.isPrimary ? "Local" : candidate.label,
        isPrimary: candidate.isPrimary,
        isLocal: isLocalRailCandidate(candidate),
        avatar: environmentLetterAvatar(candidate.label),
        status: resolveEnvironmentRailStatus(candidate),
        statusLabel: statusLabel(candidate),
        available: candidate.phase === "connected",
      },
    ]),
  );
}

function folderName(workspaceRoot: string): string {
  return workspaceRoot.split(/[\\/]/).findLast((segment) => segment.length > 0) ?? workspaceRoot;
}

// `projects` must be built with `separate` grouping: one node per physical checkout.
// Timestamp orders keep the input order, so `projects` must already be sorted by that timestamp.
export function groupProjectsByRepository(input: {
  readonly projects: readonly SidebarProjectSnapshot[];
  readonly environments: ReadonlyMap<EnvironmentId, EnvironmentCardIdentity>;
  readonly sortOrder: SidebarRepositorySortOrder;
}): RepositoryGroup[] {
  const buckets = new Map<string, SidebarProjectSnapshot[]>();
  for (const project of input.projects) {
    const key = project.repositoryIdentity?.canonicalKey ?? project.projectKey;
    const bucket = buckets.get(key);
    if (bucket) bucket.push(project);
    else buckets.set(key, [project]);
  }
  // Primary Local, then desktop-local environments, then remotes.
  const rank = (project: SidebarProjectSnapshot) => {
    const environment = input.environments.get(project.environmentId);
    return environment?.isPrimary ? 0 : environment?.isLocal ? 1 : 2;
  };
  const label = (project: SidebarProjectSnapshot) =>
    input.environments.get(project.environmentId)?.label ?? project.environmentId;
  const groups = [...buckets].map(([key, projects]) => {
    const identity = projects[0]!.repositoryIdentity ?? null;
    const cards = projects.toSorted(
      (left, right) =>
        rank(left) - rank(right) ||
        compareSidebarDisplayText(label(left), label(right)) ||
        compareSidebarDisplayText(left.workspaceRoot, right.workspaceRoot),
    );
    return {
      key,
      title: identity?.name ?? identity?.displayName ?? folderName(projects[0]!.workspaceRoot),
      host: identity ? (identity.canonicalKey.split("/")[0] ?? null) : null,
      environmentCount: new Set(cards.map((card) => card.environmentId)).size,
      cards,
    };
  });
  const titleCounts = new Map<string, number>();
  for (const group of groups) {
    const folded = group.title.toLowerCase();
    titleCounts.set(folded, (titleCounts.get(folded) ?? 0) + 1);
  }
  const ordered =
    input.sortOrder === "name"
      ? groups.toSorted(
          (left, right) =>
            compareSidebarDisplayText(left.title.toLowerCase(), right.title.toLowerCase()) ||
            compareSidebarDisplayText(left.key, right.key),
        )
      : groups;
  return ordered.map((group) => ({
    ...group,
    showHost: group.host !== null && (titleCounts.get(group.title.toLowerCase()) ?? 0) > 1,
  }));
}
