import { deriveLogicalProjectKey } from "@coddy/client-runtime/state/project-grouping";

import type { Project } from "./types";

/**
 * Every logical key a project's unsent draft may be stored under across grouping
 * modes and identity changes (physical, repository, repository path), minus the
 * key currently in use.
 */
export function projectDraftFallbackKeys(
  project: Pick<Project, "id" | "environmentId" | "workspaceRoot" | "repositoryIdentity">,
  logicalProjectKey: string,
): string[] {
  const keys = (["separate", "repository", "repository_path"] as const).map((groupingMode) =>
    deriveLogicalProjectKey(project, { groupingMode }),
  );
  return [...new Set(keys)].filter((key) => key !== logicalProjectKey);
}

export {
  deriveLogicalProjectKey,
  deriveLogicalProjectKeyFromRef,
  deriveLogicalProjectKeyFromSettings,
  derivePhysicalProjectKey,
  derivePhysicalProjectKeyFromPath,
  deriveProjectGroupLabel,
  deriveProjectGroupingOverrideKey,
  getProjectOrderKey,
  resolveProjectGroupingMode,
  selectProjectGroupingSettings,
  type ProjectGroupingMode,
  type ProjectGroupingSettings,
} from "@coddy/client-runtime/state/project-grouping";
