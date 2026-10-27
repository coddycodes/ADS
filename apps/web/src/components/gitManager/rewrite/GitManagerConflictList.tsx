import type { GitManagerBlockedReason, GitManagerConflictState } from "@coddy/contracts";
import { AlertTriangleIcon, CheckCircle2Icon, ChevronDownIcon, Undo2Icon } from "lucide-react";
import { memo, useCallback, useState } from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";

import {
  hasLiveConflictMarkers,
  isConflictResolved,
  resolveConflictCount,
} from "./GitManagerConflictList.logic";

export interface GitManagerConflictListProps {
  readonly conflicts: ReadonlyArray<GitManagerConflictState>;
  readonly onResolve: (path: string, side: "ours" | "theirs") => void;
  readonly onUndoResolve: (path: string) => void;
  readonly continueBlocked: GitManagerBlockedReason | null;
  readonly disabledReason?: string | null;
  readonly onContinue?: () => void;
  readonly onCommit?: () => void;
}

interface GitManagerConflictRowProps {
  readonly conflict: GitManagerConflictState;
  readonly disabledReason: string | null;
  readonly onResolve: (path: string, side: "ours" | "theirs") => void;
  readonly onUndoResolve: (path: string) => void;
}

function conflictRowPropsEqual(
  previous: Readonly<GitManagerConflictRowProps>,
  next: Readonly<GitManagerConflictRowProps>,
): boolean {
  return (
    previous.conflict.path === next.conflict.path &&
    previous.conflict.kind === next.conflict.kind &&
    previous.conflict.markerCount === next.conflict.markerCount &&
    previous.conflict.resolution === next.conflict.resolution &&
    previous.disabledReason === next.disabledReason &&
    previous.onResolve === next.onResolve &&
    previous.onUndoResolve === next.onUndoResolve
  );
}

const GitManagerConflictRow = memo(function GitManagerConflictRow({
  conflict,
  disabledReason,
  onResolve,
  onUndoResolve,
}: GitManagerConflictRowProps) {
  const resolved = isConflictResolved(conflict);
  const conflictCount = resolveConflictCount(conflict.markerCount);
  const resolveOurs = useCallback(
    () => onResolve(conflict.path, "ours"),
    [conflict.path, onResolve],
  );
  const resolveTheirs = useCallback(
    () => onResolve(conflict.path, "theirs"),
    [conflict.path, onResolve],
  );
  const undo = useCallback(() => onUndoResolve(conflict.path), [conflict.path, onUndoResolve]);
  const disabledReasonId =
    disabledReason === null
      ? undefined
      : `git-manager-conflict-${encodeURIComponent(conflict.path)}-disabled-reason`;

  return (
    <li className="flex min-w-0 items-center gap-2 border-b border-border/60 px-3 py-2 text-xs">
      {resolved ? (
        <CheckCircle2Icon aria-hidden="true" className="size-4 shrink-0 text-emerald-600" />
      ) : (
        <AlertTriangleIcon aria-hidden="true" className="size-4 shrink-0 text-amber-600" />
      )}
      <span className="min-w-0 flex-1 truncate font-mono" title={conflict.path} translate="no">
        {conflict.path}
      </span>
      <span className="shrink-0 text-muted-foreground">
        {resolved
          ? "Resolved"
          : `${conflictCount} ${conflictCount === 1 ? "conflict" : "conflicts"}`}
      </span>
      {resolved ? (
        <Button
          aria-describedby={disabledReasonId}
          aria-label={`Undo resolution for ${conflict.path}`}
          disabled={disabledReason !== null}
          size="xs"
          title={disabledReason ?? undefined}
          variant="ghost"
          onClick={undo}
        >
          <Undo2Icon aria-hidden="true" />
          Undo
        </Button>
      ) : conflict.kind === "binary" || conflict.kind === "submodule" ? (
        <details className="relative">
          <summary
            aria-describedby={disabledReasonId}
            aria-label={`Resolve ${conflict.path}`}
            className="inline-flex min-h-7 cursor-pointer list-none items-center gap-1 rounded-md border border-border px-2 hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
            title={disabledReason ?? undefined}
          >
            Resolve
            <ChevronDownIcon aria-hidden="true" className="size-3" />
          </summary>
          <div className="absolute end-0 z-20 mt-1 min-w-28 rounded-md border border-border bg-popover p-1 shadow-lg">
            <Button
              aria-describedby={disabledReasonId}
              aria-label={`Resolve ${conflict.path} with ours`}
              className="w-full justify-start"
              disabled={disabledReason !== null}
              size="xs"
              title={disabledReason ?? undefined}
              variant="ghost"
              onClick={resolveOurs}
            >
              Ours
            </Button>
            <Button
              aria-describedby={disabledReasonId}
              aria-label={`Resolve ${conflict.path} with theirs`}
              className="w-full justify-start"
              disabled={disabledReason !== null}
              size="xs"
              title={disabledReason ?? undefined}
              variant="ghost"
              onClick={resolveTheirs}
            >
              Theirs
            </Button>
          </div>
        </details>
      ) : null}
      {disabledReason === null ? null : (
        <span className="sr-only" id={disabledReasonId}>
          {disabledReason}
        </span>
      )}
    </li>
  );
}, conflictRowPropsEqual);

export const GitManagerConflictList = memo(function GitManagerConflictList({
  conflicts,
  onResolve,
  onUndoResolve,
  continueBlocked,
  disabledReason = null,
  onContinue,
  onCommit,
}: GitManagerConflictListProps) {
  const [commitWarningOpen, setCommitWarningOpen] = useState(false);
  const requestCommit = useCallback(() => {
    if (hasLiveConflictMarkers(conflicts)) {
      setCommitWarningOpen(true);
      return;
    }
    onCommit?.();
  }, [conflicts, onCommit]);
  const confirmCommit = useCallback(() => {
    setCommitWarningOpen(false);
    onCommit?.();
  }, [onCommit]);
  const closeCommitWarning = useCallback(() => setCommitWarningOpen(false), []);
  const effectiveContinueReason = disabledReason ?? continueBlocked?.message ?? null;
  const continueReasonId =
    effectiveContinueReason === null ? undefined : "git-manager-conflicts-continue-reason";

  return (
    <section aria-label="Conflicted files" className="min-h-0">
      <ul aria-label="Files with conflicts" className="divide-y divide-border/50">
        {conflicts.map((conflict) => (
          <GitManagerConflictRow
            conflict={conflict}
            disabledReason={disabledReason}
            key={conflict.path}
            onResolve={onResolve}
            onUndoResolve={onUndoResolve}
          />
        ))}
      </ul>
      <div className="flex items-center justify-end gap-2 border-t border-border px-3 py-2">
        {onCommit === undefined ? null : (
          <Button aria-label="Commit resolved files" variant="outline" onClick={requestCommit}>
            Commit Resolved Files
          </Button>
        )}
        <Button
          aria-describedby={continueReasonId}
          aria-label="Continue operation"
          disabled={effectiveContinueReason !== null}
          title={effectiveContinueReason ?? undefined}
          onClick={onContinue}
        >
          Continue
        </Button>
        {effectiveContinueReason === null ? null : (
          <span className="sr-only" id={continueReasonId}>
            {effectiveContinueReason}
          </span>
        )}
      </div>
      <Dialog open={commitWarningOpen} onOpenChange={setCommitWarningOpen}>
        <DialogPopup>
          <DialogHeader>
            <DialogTitle>Commit files with conflict markers?</DialogTitle>
            <DialogDescription>
              One or more files still contain conflict markers. Committing now may preserve an
              incomplete resolution.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={closeCommitWarning}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={confirmCommit}>
              Commit Anyway
            </Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    </section>
  );
});
