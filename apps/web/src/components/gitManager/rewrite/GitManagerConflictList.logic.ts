import type { GitManagerConflictState } from "@coddy/contracts";

export function resolveConflictCount(markerCount: number): number {
  return Math.ceil(Math.max(0, markerCount) / 3);
}

export function isConflictResolved(conflict: GitManagerConflictState): boolean {
  return conflict.kind === "text" ? conflict.markerCount === 0 : conflict.resolution !== null;
}

export function hasLiveConflictMarkers(conflicts: ReadonlyArray<GitManagerConflictState>): boolean {
  return conflicts.some((conflict) => conflict.markerCount > 0);
}
