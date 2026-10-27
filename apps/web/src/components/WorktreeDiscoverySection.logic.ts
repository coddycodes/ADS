import type {
  EnvironmentId,
  ProjectId,
  VcsWorktreeDescriptor,
  WorktreeDiscoveryVisibility,
} from "@coddy/contracts";
import { compareSidebarDisplayText } from "../sidebarProjectGrouping";

export interface WorktreeDiscoverySource {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly projectId: ProjectId;
  readonly candidates: ReadonlyArray<VcsWorktreeDescriptor>;
}

export interface WorktreeDiscoveryCandidatePresentation {
  readonly candidate: VcsWorktreeDescriptor;
  readonly label: string;
  readonly discriminator: string | null;
}

export interface WorktreeDiscoveryParentGroup {
  readonly parentDirectory: string;
  readonly candidates: ReadonlyArray<WorktreeDiscoveryCandidatePresentation>;
}

export interface WorktreeDiscoveryEnvironmentGroup {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly projectId: ProjectId;
  readonly parentGroups: ReadonlyArray<WorktreeDiscoveryParentGroup>;
}

export interface WorktreeAddAllSummary {
  readonly type: "success" | "warning" | "error";
  readonly title: string;
  readonly description: string;
}

function getParentDirectoryForDisplay(path: string): string {
  const separatorIndex = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (separatorIndex < 0) {
    return ".";
  }
  if (separatorIndex === 0) {
    return path.slice(0, 1);
  }
  if (separatorIndex === 2 && path[1] === ":") {
    return path.slice(0, separatorIndex + 1);
  }
  return path.slice(0, separatorIndex);
}

function getFinalPathComponentForDisplay(path: string): string {
  const separatorIndex = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separatorIndex < 0 ? path : path.slice(separatorIndex + 1);
}

export function getWorktreeCandidateLabel(candidate: VcsWorktreeDescriptor): string {
  if (candidate.branch !== null) {
    return candidate.branch;
  }
  return candidate.head === null ? "Detached" : `Detached ${candidate.head.slice(0, 7)}`;
}

export function buildWorktreeDiscoveryGroups(
  sources: ReadonlyArray<WorktreeDiscoverySource>,
): WorktreeDiscoveryEnvironmentGroup[] {
  return sources
    .map((source): WorktreeDiscoveryEnvironmentGroup => {
      const candidatesByParent = new Map<string, WorktreeDiscoveryCandidatePresentation[]>();
      for (const candidate of source.candidates) {
        const parentDirectory = getParentDirectoryForDisplay(candidate.path);
        const presentation = {
          candidate,
          discriminator: null,
          label: getWorktreeCandidateLabel(candidate),
        };
        const existing = candidatesByParent.get(parentDirectory);
        if (existing) {
          existing.push(presentation);
        } else {
          candidatesByParent.set(parentDirectory, [presentation]);
        }
      }

      return {
        environmentId: source.environmentId,
        environmentLabel: source.environmentLabel,
        projectId: source.projectId,
        parentGroups: [...candidatesByParent]
          .sort(([left], [right]) => compareSidebarDisplayText(left, right))
          .map(([parentDirectory, candidates]) => {
            candidates.sort(
              (left, right) =>
                compareSidebarDisplayText(left.label, right.label) ||
                compareSidebarDisplayText(left.candidate.path, right.candidate.path),
            );
            const labelCounts = new Map<string, number>();
            for (const item of candidates) {
              labelCounts.set(item.label, (labelCounts.get(item.label) ?? 0) + 1);
            }
            return {
              parentDirectory,
              candidates: candidates.map((item) => ({
                ...item,
                discriminator:
                  (labelCounts.get(item.label) ?? 0) > 1
                    ? getFinalPathComponentForDisplay(item.candidate.path)
                    : null,
              })),
            };
          }),
      };
    })
    .sort(
      (left, right) =>
        compareSidebarDisplayText(left.environmentLabel, right.environmentLabel) ||
        compareSidebarDisplayText(left.environmentId, right.environmentId) ||
        compareSidebarDisplayText(left.projectId, right.projectId),
    );
}

export function formatDiscoveredWorktreeCount(count: number): string {
  return `${count} discovered worktree${count === 1 ? "" : "s"}`;
}

export function formatWorktreeAddAllSummary(input: {
  readonly successCount: number;
  readonly failureCount: number;
}): WorktreeAddAllSummary {
  const totalCount = input.successCount + input.failureCount;
  if (input.failureCount === 0) {
    return {
      type: "success",
      title: `Added ${formatDiscoveredWorktreeCount(input.successCount)}`,
      description: "All discovered worktrees were added to Coddy Studio.",
    };
  }
  if (input.successCount === 0) {
    return {
      type: "error",
      title: `Could not add ${formatDiscoveredWorktreeCount(input.failureCount)}`,
      description: "No discovered worktrees were added.",
    };
  }
  return {
    type: "warning",
    title: `Added ${input.successCount} of ${totalCount} discovered worktrees`,
    description: `${input.failureCount} worktree${input.failureCount === 1 ? "" : "s"} could not be added.`,
  };
}

/**
 * The project menu's discovery toggle. `hiddenCount` comes from the mounted
 * discovery section; null (collapsed project, not loaded) omits the count.
 */
export function getDiscoveryVisibilityMenuLabel(
  visibility: WorktreeDiscoveryVisibility,
  hiddenCount: number | null = null,
): string {
  if (visibility !== "hidden") {
    return "Hide Discovered Worktrees";
  }
  return hiddenCount === null ? "Show Hidden Worktrees" : `Show Hidden Worktrees (${hiddenCount})`;
}
