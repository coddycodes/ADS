import type { VcsAdoptedWorktreeStatus } from "@coddy/contracts";
import { AlertTriangleIcon, RefreshCwIcon, Trash2Icon } from "lucide-react";
import type { MouseEvent } from "react";

import { Button } from "./ui/button";

export interface WorktreeAvailabilityWarningProps {
  readonly status: VcsAdoptedWorktreeStatus;
  readonly onRetry: () => void;
  readonly onRemove: () => void;
}

export function WorktreeAvailabilityWarning({
  status,
  onRetry,
  onRemove,
}: WorktreeAvailabilityWarningProps) {
  if (status.availability === "present") return null;

  const stopAndRun = (action: () => void) => (event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    event.stopPropagation();
    action();
  };
  const summary =
    status.availability === "missing-registered"
      ? "The worktree directory is missing. Git registration remains."
      : status.availability === "missing-unregistered"
        ? "The worktree directory is missing and Git no longer registers this worktree."
        : status.availability === "verification-unavailable"
          ? "This worktree could not be verified. Its last known state is retained."
          : "Removal is in progress.";
  const registrationState =
    status.registrationState === "registered"
      ? "Registered"
      : status.registrationState === "prunable"
        ? "Prunable (stale)"
        : status.availability === "missing-unregistered"
          ? "Not registered"
          : "Unknown";

  return (
    <div
      role={status.availability === "removing" ? "status" : "alert"}
      className="mt-1 space-y-1.5 rounded-md border border-warning/30 bg-warning/8 p-2 text-xs"
      data-testid={`worktree-availability-${status.threadId}`}
    >
      <p className="flex items-start gap-1.5 font-medium text-warning-foreground">
        <AlertTriangleIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
        <span>{summary}</span>
      </p>
      <dl className="grid gap-0.5 text-muted-foreground">
        <div>
          <dt className="inline font-medium">Last-known branch: </dt>
          <dd className="inline">{status.branch ?? "Detached HEAD"}</dd>
        </div>
        <div>
          <dt className="sr-only">Full path</dt>
          <dd className="break-all font-mono text-xs">{status.path}</dd>
        </div>
        <div>
          <dt className="inline font-medium">Git registration: </dt>
          <dd className="inline">{registrationState}</dd>
        </div>
        {status.locked ? (
          <div>
            <dt className="inline font-medium">Locked: </dt>
            <dd className="inline">{status.lockReason ?? "Git did not provide a reason."}</dd>
          </div>
        ) : null}
      </dl>
      {status.availability !== "removing" ? (
        <div className="flex flex-wrap gap-1.5">
          <Button type="button" size="xs" variant="outline" onClick={stopAndRun(onRetry)}>
            <RefreshCwIcon aria-hidden className="size-3" />
            Retry detection
          </Button>
          <Button type="button" size="xs" variant="outline" onClick={stopAndRun(onRemove)}>
            <Trash2Icon aria-hidden className="size-3" />
            Remove from Coddy Studio
          </Button>
        </div>
      ) : null}
    </div>
  );
}
