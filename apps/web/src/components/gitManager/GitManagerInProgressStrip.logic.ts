import type { GitManagerBlockedReason, GitManagerInProgressOperation } from "@coddy/contracts";

export interface InProgressOperationPresentation {
  readonly label: string;
  readonly actionLabel: string;
  readonly progress: string | null;
}

export function describeInProgressOperation(
  operation: GitManagerInProgressOperation,
): InProgressOperationPresentation {
  const labels: Record<GitManagerInProgressOperation["kind"], string> = {
    merge: "Merge",
    rebase: "Rebase",
    "cherry-pick": "Cherry-pick",
    revert: "Revert",
    squash: "Squash merge",
  };
  const actionLabels: Record<GitManagerInProgressOperation["kind"], string> = {
    merge: "Merge",
    rebase: "Rebase",
    "cherry-pick": "Cherry-pick",
    revert: "Revert",
    squash: "Squash Merge",
  };
  return {
    label: `${labels[operation.kind]} underway`,
    actionLabel: actionLabels[operation.kind],
    progress:
      operation.current === null || operation.total === null
        ? null
        : `Step ${operation.current} of ${operation.total}`,
  };
}

export function resolveInProgressBlockedReason(
  blocked: GitManagerBlockedReason | null,
): string | null {
  return blocked?.message ?? null;
}
