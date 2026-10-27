import type { VcsWorkingTreeFileStatus } from "@coddy/contracts";

interface WorkingTreeStatusBadge {
  readonly letter: string;
  readonly className: string;
  readonly label: string;
}

export const WORKING_TREE_STATUS_BADGE: Record<VcsWorkingTreeFileStatus, WorkingTreeStatusBadge> = {
  modified: { letter: "M", className: "text-warning", label: "Modified" },
  added: { letter: "A", className: "text-success", label: "Added" },
  deleted: { letter: "D", className: "text-destructive", label: "Deleted" },
  renamed: { letter: "R", className: "text-warning", label: "Renamed" },
  copied: { letter: "C", className: "text-success", label: "Copied" },
  untracked: { letter: "U", className: "text-success", label: "Untracked" },
  conflicted: { letter: "!", className: "text-destructive", label: "Conflicted" },
};

export function workingTreeStatusBadge(
  status: VcsWorkingTreeFileStatus | undefined,
): WorkingTreeStatusBadge {
  return WORKING_TREE_STATUS_BADGE[status ?? "modified"];
}
