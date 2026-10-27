import { AlertTriangleIcon } from "lucide-react";
import type { GitManagerRefEntry } from "@coddy/contracts";
import { memo, useCallback, useMemo } from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";

import { GitManagerOperationBanner } from "../toolbar/GitManagerOperationBanner";
import { groupBranches } from "../toolbar/branchGrouping";
import { summarizeMergePreview } from "../merge/GitManagerMergeDialog.logic";
import { GitManagerConflictList } from "./GitManagerConflictList";
import type {
  GitManagerMultiCommitEvent,
  GitManagerMultiCommitState,
} from "./gitManagerMultiCommitOperation.logic";

const EMPTY_REFS: ReadonlyArray<GitManagerRefEntry> = Object.freeze([]);
const EMPTY_RECENT_NAMES: ReadonlyArray<string> = Object.freeze([]);

interface BranchChoiceProps {
  readonly branch: GitManagerRefEntry;
  readonly operation: string;
  readonly disabledReason: string | null;
  readonly onChoose: (branch: string) => void;
}

const BranchChoice = memo(function BranchChoice({
  branch,
  operation,
  disabledReason,
  onChoose,
}: BranchChoiceProps) {
  const choose = useCallback(() => onChoose(branch.name), [branch.name, onChoose]);
  const blocked = branch.blocked.find((reason) => reason.operation === operation) ?? null;
  const effectiveDisabledReason = disabledReason ?? blocked?.message ?? null;
  const reasonId =
    effectiveDisabledReason === null
      ? undefined
      : `git-manager-branch-choice-${branch.tipSha}-reason`;
  return (
    <>
      <Button
        aria-describedby={reasonId}
        aria-label={`Choose branch ${branch.name}`}
        className="w-full min-w-0 justify-start font-mono"
        disabled={effectiveDisabledReason !== null}
        size="sm"
        title={effectiveDisabledReason ?? undefined}
        variant="ghost"
        onClick={choose}
      >
        <span className="truncate" translate="no">
          {branch.name}
        </span>
      </Button>
      {effectiveDisabledReason === null ? null : (
        <span className="sr-only" id={reasonId}>
          {effectiveDisabledReason}
        </span>
      )}
    </>
  );
});

interface BranchChoiceGroupProps {
  readonly label: string;
  readonly branches: ReadonlyArray<GitManagerRefEntry>;
  readonly operation: string;
  readonly disabledReason: string | null;
  readonly onChoose: (branch: string) => void;
}

const BranchChoiceGroup = memo(function BranchChoiceGroup({
  label,
  branches,
  operation,
  disabledReason,
  onChoose,
}: BranchChoiceGroupProps) {
  if (branches.length === 0) return null;
  return (
    <section aria-label={`${label} branches`}>
      <h3 className="px-2 py-1 text-xs font-semibold text-muted-foreground uppercase">{label}</h3>
      {branches.map((branch) => (
        <BranchChoice
          branch={branch}
          disabledReason={disabledReason}
          key={branch.name}
          operation={operation}
          onChoose={onChoose}
        />
      ))}
    </section>
  );
});

export interface GitManagerMultiCommitOperationDialogProps {
  readonly state: GitManagerMultiCommitState;
  readonly disabledReason?: string | null;
  readonly onAdvance: (event: GitManagerMultiCommitEvent) => void;
  readonly onCancel: () => void;
  readonly onConfirmAbort: () => void;
}

function operationTitle(state: GitManagerMultiCommitState): string {
  const operation =
    state.kind === "cherry-pick"
      ? "Cherry-Pick"
      : state.kind.charAt(0).toUpperCase() + state.kind.slice(1);
  switch (state.step) {
    case "warn-force-push":
      return `Rewrite ${operation} History?`;
    case "confirm-abort":
      return `Abort ${operation}?`;
    case "show-progress":
      return `${operation} in Progress`;
    case "show-conflicts":
      return `Resolve ${operation} Conflicts`;
    case "choose-branch":
      return `Choose a Branch to ${operation}`;
    case "create-branch":
      return "Create a Branch";
    case "hide-conflicts":
    case null:
      return operation;
  }
}

export const GitManagerMultiCommitOperationDialog = memo(
  function GitManagerMultiCommitOperationDialog({
    state,
    disabledReason = null,
    onAdvance,
    onCancel,
    onConfirmAbort,
  }: GitManagerMultiCommitOperationDialogProps) {
    const confirmForcePush = useCallback(() => {
      if (disabledReason === null) onAdvance({ _tag: "force-push-confirmed" });
    }, [disabledReason, onAdvance]);
    const chooseBranch = useCallback(
      (branch: string) => {
        if (disabledReason !== null) return;
        onAdvance({
          _tag: "branch-chosen",
          branch,
          commitsArePushed: state.commitsArePushed === true,
        });
      },
      [disabledReason, onAdvance, state.commitsArePushed],
    );
    const resolveConflict = useCallback(
      (path: string, side: "ours" | "theirs") =>
        onAdvance({ _tag: "resolve-conflict-requested", path, side }),
      [onAdvance],
    );
    const undoConflictResolution = useCallback(
      (path: string) => onAdvance({ _tag: "undo-conflict-resolution-requested", path }),
      [onAdvance],
    );
    const continueOperation = useCallback(
      () => onAdvance({ _tag: "continue-requested" }),
      [onAdvance],
    );
    const dismissConflicts = useCallback(
      () => onAdvance({ _tag: "dismiss-conflicts" }),
      [onAdvance],
    );
    const viewConflicts = useCallback(() => onAdvance({ _tag: "view-conflicts" }), [onAdvance]);
    const closeDialog = useCallback(
      (open: boolean) => {
        if (open) return;
        if (state.step === "show-conflicts") {
          onAdvance({ _tag: "dismiss-conflicts" });
          return;
        }
        onCancel();
      },
      [onAdvance, onCancel, state.step],
    );

    const groupedBranches = useMemo(
      () =>
        groupBranches({
          refs: state.refs ?? EMPTY_REFS,
          recentNames: state.recentNames ?? EMPTY_RECENT_NAMES,
          filter: "",
        }),
      [state.recentNames, state.refs],
    );
    const mergeSummary = state.mergePreview ? summarizeMergePreview(state.mergePreview) : null;
    const progress = state.inProgressOperation;
    const progressText =
      progress?.current === null || progress?.current === undefined || progress.total === null
        ? null
        : `Commit ${progress.current} of ${progress.total}`;
    const disabledReasonId =
      disabledReason === null ? undefined : "git-manager-multi-commit-disabled-reason";

    if (state.step === null) return null;
    if (state.step === "hide-conflicts") {
      return (
        <section
          aria-atomic="true"
          className="flex items-center gap-2 border-b border-amber-500/35 bg-amber-500/10 px-3 py-2 text-xs"
          role="alert"
        >
          <AlertTriangleIcon aria-hidden="true" className="size-4 shrink-0 text-amber-600" />
          <span className="min-w-0 flex-1">The repository operation is waiting on conflicts.</span>
          <Button size="xs" variant="outline" onClick={viewConflicts}>
            View Conflicts
          </Button>
        </section>
      );
    }

    return (
      <Dialog open onOpenChange={closeDialog}>
        <DialogPopup className="max-w-2xl" showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>{operationTitle(state)}</DialogTitle>
            <DialogDescription>
              {state.step === "warn-force-push"
                ? "History will be rewritten, and a force push will be needed to update the remote branch. The server always uses force-with-lease."
                : state.step === "confirm-abort"
                  ? "Abort the repository operation and discard its in-progress rewrite state?"
                  : "Review the repository operation state reported by the server."}
            </DialogDescription>
          </DialogHeader>
          <div className="min-h-0 space-y-3 px-6 pb-4">
            {disabledReason === null ? null : (
              <p className="text-xs text-muted-foreground" id={disabledReasonId}>
                {disabledReason}
              </p>
            )}
            {state.step === "warn-force-push" ? (
              <div className="flex items-start gap-2 rounded-md border border-destructive/35 bg-destructive/5 p-3 text-sm">
                <AlertTriangleIcon aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
                <p>
                  Rewriting pushed commits changes their identifiers. Confirm only if replacing the
                  remote branch history is intended.
                </p>
              </div>
            ) : null}
            {state.step === "show-progress" ? (
              <>
                {progressText === null ? null : (
                  <p className="text-sm font-medium tabular-nums">{progressText}</p>
                )}
                <GitManagerOperationBanner operation={state.operationEvent} onCancel={onCancel} />
              </>
            ) : null}
            {state.step === "choose-branch" ? (
              <>
                <div
                  aria-label="Rewrite target branches"
                  className="max-h-64 overflow-auto rounded-md border border-border p-1"
                >
                  <BranchChoiceGroup
                    branches={groupedBranches.default}
                    disabledReason={disabledReason}
                    label="Default"
                    operation={state.kind}
                    onChoose={chooseBranch}
                  />
                  <BranchChoiceGroup
                    branches={groupedBranches.recent}
                    disabledReason={disabledReason}
                    label="Recent"
                    operation={state.kind}
                    onChoose={chooseBranch}
                  />
                  <BranchChoiceGroup
                    branches={groupedBranches.other}
                    disabledReason={disabledReason}
                    label="Other"
                    operation={state.kind}
                    onChoose={chooseBranch}
                  />
                </div>
                {mergeSummary === null ? null : (
                  <div aria-live="polite" className="rounded-md bg-muted/35 p-3 text-xs">
                    <p>{mergeSummary.message}</p>
                    <p className="mt-1 text-xs text-muted-foreground tabular-nums">
                      Ahead {mergeSummary.ahead} · Behind {mergeSummary.behind}
                    </p>
                  </div>
                )}
              </>
            ) : null}
            {state.step === "show-conflicts" ? (
              <GitManagerConflictList
                conflicts={state.conflicts}
                continueBlocked={state.continueBlocked}
                disabledReason={disabledReason}
                onContinue={continueOperation}
                onResolve={resolveConflict}
                onUndoResolve={undoConflictResolution}
              />
            ) : null}
          </div>
          <DialogFooter>
            {state.step === "warn-force-push" ? (
              <>
                <Button variant="outline" onClick={onCancel}>
                  Cancel
                </Button>
                <Button
                  aria-describedby={disabledReasonId}
                  disabled={disabledReason !== null}
                  title={disabledReason ?? undefined}
                  variant="destructive"
                  onClick={confirmForcePush}
                >
                  Rewrite History
                </Button>
              </>
            ) : state.step === "confirm-abort" ? (
              <>
                <Button variant="outline" onClick={onCancel}>
                  Keep Working
                </Button>
                <Button
                  aria-describedby={disabledReasonId}
                  disabled={disabledReason !== null}
                  title={disabledReason ?? undefined}
                  variant="destructive"
                  onClick={onConfirmAbort}
                >
                  Abort{" "}
                  {state.kind === "cherry-pick"
                    ? "Cherry-Pick"
                    : operationTitle(state).slice(6, -1)}
                </Button>
              </>
            ) : state.step === "show-conflicts" ? (
              <Button variant="outline" onClick={dismissConflicts}>
                Hide Conflicts
              </Button>
            ) : null}
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    );
  },
);
