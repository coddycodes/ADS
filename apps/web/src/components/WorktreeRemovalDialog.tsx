import { scopeProjectRef, scopeThreadRef } from "@bibcode/client-runtime/environment";
import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/shell";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import {
  WorktreeRemovalError,
  type AdoptedWorktreeAvailability,
  type EnvironmentId,
  type ProjectId,
  type ScopedProjectRef,
  type ThreadId,
  type VcsWorktreeRegistrationState,
  type WorktreeRemovalMode,
  type WorktreeRemovalPlan,
  type WorktreeRemovalResult,
} from "@bibcode/contracts";
import * as Schema from "effect/Schema";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import { newCommandId } from "../lib/utils";
import {
  readEnvironmentThreadRefs,
  readThreadShell,
  useThreadShell,
  useThreadShellsForProjectRefs,
} from "../state/entities";
import { worktreeEnvironment } from "../state/worktrees";
import { useAtomCommand } from "../state/use-atom-command";
import { isWorktreeSessionRunning } from "./Sidebar.logic";
import { WORKTREE_DELETE_BLOCKED_REASON } from "./sidebar/sidebarMenus.logic";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";

export interface WorktreeRemovalTarget {
  readonly environmentId: EnvironmentId;
  readonly projectId: ProjectId;
  readonly threadId: ThreadId;
  readonly title: string;
  readonly path: string;
  readonly branch: string | null;
  readonly availability: AdoptedWorktreeAvailability;
  readonly registrationState: VcsWorktreeRegistrationState | null;
  readonly locked: boolean;
  readonly lockReason?: string;
}

export interface WorktreeRemovalDialogProps {
  readonly open: boolean;
  readonly target: WorktreeRemovalTarget | null;
  readonly onOpenChange: (open: boolean) => void;
  readonly onRemoved: (target: WorktreeRemovalTarget, result: WorktreeRemovalResult) => void;
}

type ConfirmationStep = "choices" | "dirty" | "prune";

const isWorktreeRemovalError = Schema.is(WorktreeRemovalError);

function isSessionRunningRefusal(result: { readonly cause: unknown }): boolean {
  const error = squashAtomCommandFailure(result as never);
  return isWorktreeRemovalError(error) && error.reason === "session-running";
}

function failureMessage(result: { readonly cause: unknown }): string {
  const error = squashAtomCommandFailure(result as never);
  if (isWorktreeRemovalError(error) && error.reason === "session-running") {
    return WORKTREE_DELETE_BLOCKED_REASON;
  }
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : "The removal request failed.";
}

function plural(count: number, singular: string, pluralValue = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralValue}`;
}

const NO_PROJECT_REFS: ReadonlyArray<ScopedProjectRef> = [];

type WorktreeSessionCard = Pick<
  EnvironmentThreadShell,
  "environmentId" | "projectId" | "worktreePath" | "archivedAt" | "session"
>;

/**
 * The target as `isWorktreeSessionRunning`'s card: its live thread or, for an
 * archived thread, which the live store leaves out (Settings → Archived
 * Threads), its checkout without a session of its own, since archiving requires
 * no active turn. The rule never reads the card's own `archivedAt`.
 */
function worktreeSessionCard(
  target: WorktreeRemovalTarget,
  liveThread: EnvironmentThreadShell | null,
): WorktreeSessionCard {
  return (
    liveThread ?? {
      environmentId: target.environmentId,
      projectId: target.projectId,
      worktreePath: target.path,
      archivedAt: null,
      session: null,
    }
  );
}

/** The same rule over the store as it is now, for the click-time re-check. */
function readWorktreeSessionRunning(target: WorktreeRemovalTarget): boolean {
  const projectThreads = readEnvironmentThreadRefs(target.environmentId).flatMap((ref) => {
    const thread = readThreadShell(ref);
    return thread !== null && thread.projectId === target.projectId ? [thread] : [];
  });
  const liveThread = projectThreads.find((thread) => thread.id === target.threadId) ?? null;
  return isWorktreeSessionRunning(worktreeSessionCard(target, liveThread), projectThreads);
}

export function WorktreeRemovalDialog({
  open,
  target,
  onOpenChange,
  onRemoved,
}: WorktreeRemovalDialogProps) {
  const getRemovalPlan = useAtomCommand(worktreeEnvironment.getRemovalPlan, {
    reportFailure: false,
  });
  const removeFromBibCode = useAtomCommand(worktreeEnvironment.removeFromBibCode, {
    reportFailure: false,
  });
  const remove = useAtomCommand(worktreeEnvironment.remove, { reportFailure: false });
  const [plan, setPlan] = useState<WorktreeRemovalPlan | null>(null);
  const [step, setStep] = useState<ConfirmationStep>("choices");
  const [isLoadingPlan, setIsLoadingPlan] = useState(
    () => open && target !== null && target.availability !== "removing",
  );
  const [isRemoving, setIsRemoving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [serverRefusedRunning, setServerRefusedRunning] = useState(false);
  const [planChanged, setPlanChanged] = useState(false);
  const [completed, setCompleted] = useState<WorktreeRemovalResult | null>(null);
  const asyncEpochRef = useRef(0);
  const deletionBlockedReasonId = useId();

  // Deletion waits while a session runs in the worktree, by the card menu's
  // rule. The store re-renders the dialog, so it re-enables once the session
  // stops; the rule runs only when the target or the project's threads change.
  const targetThreadRef = useMemo(
    () => (target === null ? null : scopeThreadRef(target.environmentId, target.threadId)),
    [target],
  );
  const targetProjectRefs = useMemo(
    () =>
      target === null ? NO_PROJECT_REFS : [scopeProjectRef(target.environmentId, target.projectId)],
    [target],
  );
  const liveTargetThread = useThreadShell(targetThreadRef);
  const projectThreads = useThreadShellsForProjectRefs(targetProjectRefs);
  const worktreeSessionRunning = useMemo(
    () =>
      target !== null &&
      isWorktreeSessionRunning(worktreeSessionCard(target, liveTargetThread), projectThreads),
    [liveTargetThread, projectThreads, target],
  );

  // Once the store catches up, the reason follows live state through session stop.
  if (serverRefusedRunning && worktreeSessionRunning) {
    setServerRefusedRunning(false);
  }

  // Callers mark the plan as loading before the request starts.
  const loadPlanForTarget = useCallback(
    (requestTarget: WorktreeRemovalTarget, epoch: number) =>
      getRemovalPlan({
        environmentId: requestTarget.environmentId,
        input: { projectId: requestTarget.projectId, threadId: requestTarget.threadId },
      }).then((result) => {
        if (epoch !== asyncEpochRef.current) return;
        setIsLoadingPlan(false);
        if (result._tag === "Failure") {
          setError(failureMessage(result));
          return;
        }
        setPlan(result.value);
      }),
    [getRemovalPlan],
  );

  // Opening, closing, or retargeting starts over from the choices.
  const [shownFor, setShownFor] = useState({ open, target });
  if (shownFor.open !== open || shownFor.target !== target) {
    setShownFor({ open, target });
    setPlan(null);
    setStep("choices");
    setIsLoadingPlan(open && target !== null && target.availability !== "removing");
    setIsRemoving(false);
    setError(null);
    setServerRefusedRunning(false);
    setPlanChanged(false);
    setCompleted(null);
  }

  useEffect(() => {
    const epoch = asyncEpochRef.current + 1;
    asyncEpochRef.current = epoch;
    if (open && target && target.availability !== "removing") {
      void loadPlanForTarget(target, epoch);
    }
    return () => {
      if (asyncEpochRef.current === epoch) {
        asyncEpochRef.current += 1;
      }
    };
  }, [loadPlanForTarget, open, target]);

  const retryLoadPlan = useCallback(() => {
    if (!target || target.availability === "removing") return;
    const epoch = asyncEpochRef.current + 1;
    asyncEpochRef.current = epoch;
    setIsLoadingPlan(true);
    setError(null);
    setServerRefusedRunning(false);
    void loadPlanForTarget(target, epoch);
  }, [loadPlanForTarget, target]);

  const finishRemoval = useCallback(
    (
      requestTarget: WorktreeRemovalTarget,
      result: WorktreeRemovalResult,
      showCompletion: boolean,
    ) => {
      if (showCompletion) setCompleted(result);
      onRemoved(requestTarget, result);
    },
    [onRemoved],
  );

  const detach = useCallback(async () => {
    if (!target || isRemoving) return;
    const requestTarget = target;
    const epoch = asyncEpochRef.current + 1;
    asyncEpochRef.current = epoch;
    setIsRemoving(true);
    setError(null);
    setServerRefusedRunning(false);
    const result = await removeFromBibCode({
      environmentId: requestTarget.environmentId,
      input: {
        commandId: newCommandId(),
        projectId: requestTarget.projectId,
        threadId: requestTarget.threadId,
      },
    });
    const isCurrent = epoch === asyncEpochRef.current;
    if (isCurrent) setIsRemoving(false);
    if (result._tag === "Failure") {
      if (isCurrent) setError(failureMessage(result));
      return;
    }
    finishRemoval(requestTarget, result.value, isCurrent);
  }, [finishRemoval, isRemoving, removeFromBibCode, target]);

  const executeDestructiveRemoval = useCallback(
    async (mode: WorktreeRemovalMode, forceDirty: boolean, confirmPrune: boolean) => {
      if (!target || !plan || isRemoving) return;
      // Re-check at click time: a session may have started since this render.
      if (readWorktreeSessionRunning(target)) return;
      const requestTarget = target;
      const requestPlan = plan;
      const epoch = asyncEpochRef.current + 1;
      asyncEpochRef.current = epoch;
      setIsRemoving(true);
      setError(null);
      setServerRefusedRunning(false);
      setPlanChanged(false);
      const result = await remove({
        environmentId: requestTarget.environmentId,
        input: {
          commandId: newCommandId(),
          projectId: requestTarget.projectId,
          threadId: requestTarget.threadId,
          mode,
          expectedGeneration: requestPlan.generation,
          planToken: requestPlan.planToken,
          forceDirty,
          confirmRepositoryWidePrune: confirmPrune,
        },
      });
      const isCurrent = epoch === asyncEpochRef.current;
      if (isCurrent) setIsRemoving(false);
      if (result._tag === "Failure") {
        if (isCurrent) {
          if (isSessionRunningRefusal(result)) {
            setServerRefusedRunning(true);
          } else {
            setError(failureMessage(result));
          }
        }
        return;
      }
      if (result.value._tag === "PlanChanged") {
        if (!isCurrent) return;
        setPlan(result.value.plan);
        setPlanChanged(true);
        setStep("choices");
        return;
      }
      finishRemoval(requestTarget, result.value.result, isCurrent);
    },
    [finishRemoval, isRemoving, plan, remove, target],
  );

  const beginDestructiveRemoval = useCallback(() => {
    if (!plan) return;
    if (plan.trackedChangeCount > 0 || plan.untrackedFileCount > 0) {
      setStep("dirty");
      return;
    }
    if (plan.pruneImpact.length > 0) {
      setStep("prune");
      return;
    }
    void executeDestructiveRemoval(
      plan.availability === "missing-registered"
        ? "cleanup-stale-registration"
        : "delete-git-worktree",
      false,
      false,
    );
  }, [executeDestructiveRemoval, plan]);

  const confirmDirty = useCallback(() => {
    if (!plan) return;
    if (plan.pruneImpact.length > 0) {
      setStep("prune");
      return;
    }
    void executeDestructiveRemoval(
      plan.availability === "missing-registered"
        ? "cleanup-stale-registration"
        : "delete-git-worktree",
      true,
      false,
    );
  }, [executeDestructiveRemoval, plan]);

  const close = useCallback(() => onOpenChange(false), [onOpenChange]);
  const handleOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (!nextOpen && isRemoving) return;
      onOpenChange(nextOpen);
    },
    [isRemoving, onOpenChange],
  );
  const removalUnavailable = target?.availability === "removing";
  const hasDirtyChanges =
    plan !== null && (plan.trackedChangeCount > 0 || plan.untrackedFileCount > 0);
  const destructiveLabel =
    plan?.availability === "missing-registered"
      ? "Clean stale Git registration and remove"
      : "Delete Git worktree and remove";
  const destructiveEligible =
    plan !== null &&
    !plan.locked &&
    (plan.availability === "present" ||
      (plan.availability === "missing-registered" && plan.registered));
  // Only a destructive button on screen waits, so only then is the reason shown.
  const deletionBlocked =
    worktreeSessionRunning && completed === null && (step !== "choices" || destructiveEligible);
  const showDeletionBlockedReason = deletionBlocked || (serverRefusedRunning && completed === null);

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogPopup className="max-w-xl" showCloseButton={!isRemoving}>
        <DialogHeader>
          <DialogTitle>Remove worktree</DialogTitle>
          <DialogDescription>
            Choose whether to remove only BiBCode history or also modify Git.
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-4">
          {target ? (
            <dl className="grid gap-2 text-sm">
              <div>
                <dt className="font-medium">Workspace</dt>
                <dd>{target.title}</dd>
              </div>
              <div>
                <dt className="font-medium">Last-known branch</dt>
                <dd>{target.branch ?? "Detached HEAD"}</dd>
              </div>
              <div>
                <dt className="font-medium">Path</dt>
                <dd className="break-all font-mono text-xs">{target.path}</dd>
              </div>
              <div>
                <dt className="font-medium">Git registration</dt>
                <dd>
                  {(plan?.registered ?? target.registrationState !== null)
                    ? "Registration remains"
                    : "Not registered"}
                </dd>
              </div>
            </dl>
          ) : null}

          {removalUnavailable ? (
            <p role="status" className="text-sm text-muted-foreground">
              Removal is already in progress.
            </p>
          ) : null}
          {isLoadingPlan ? (
            <p role="status" className="text-sm text-muted-foreground">
              Loading removal details…
            </p>
          ) : null}
          {planChanged ? (
            <p role="alert" className="text-sm text-warning">
              Removal details changed. Review the updated plan before continuing.
            </p>
          ) : null}
          {step === "choices" && hasDirtyChanges && plan ? (
            <p className="text-sm text-warning">
              Local changes detected: {plural(plan.trackedChangeCount, "tracked change")} and{" "}
              {plural(plan.untrackedFileCount, "untracked file")}.
            </p>
          ) : null}
          {error ? (
            <div role="alert" className="space-y-2 text-sm text-destructive">
              <p>{error}</p>
              <Button type="button" size="sm" variant="outline" onClick={retryLoadPlan}>
                Retry removal details
              </Button>
            </div>
          ) : null}
          {(plan?.locked || target?.locked) && !completed ? (
            <p role="alert" className="text-sm text-warning">
              Git cleanup is unavailable because this registration is locked
              {(plan?.lockReason ?? target?.lockReason)
                ? `: ${plan?.lockReason ?? target?.lockReason}`
                : "."}
            </p>
          ) : null}

          {completed ? (
            <div role="status" className="space-y-2 text-sm">
              <p className="font-medium">Removed from BiBCode</p>
              {completed.gitOutcome === "failed" || completed.orphanCleanupPending ? (
                <p className="text-warning">
                  Git cleanup still needs attention. {completed.detail ?? "Clean it up manually."}
                </p>
              ) : completed.detail ? (
                <p>{completed.detail}</p>
              ) : null}
            </div>
          ) : step === "dirty" && plan ? (
            <div role="alert" className="space-y-2 text-sm">
              <p className="font-medium">This worktree contains local changes.</p>
              <p>
                Deleting it discards {plural(plan.trackedChangeCount, "tracked change")} and{" "}
                {plural(plan.untrackedFileCount, "untracked file")}.
              </p>
            </div>
          ) : step === "prune" && plan ? (
            <div role="alert" className="space-y-2 text-sm">
              <p className="font-medium">Repository-wide prune affects these registrations:</p>
              <ul className="list-disc space-y-2 pl-5">
                {plan.pruneImpact.map((impact) => (
                  <li key={`${impact.path}:${impact.pruneReason}`}>
                    <span className="block break-all font-mono text-xs">{impact.path}</span>
                    <span>{impact.pruneReason}</span>
                    {impact.lockReason ? <span> — {impact.lockReason}</span> : null}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {showDeletionBlockedReason ? (
            <p id={deletionBlockedReasonId} role="status" className="text-sm text-warning">
              {WORKTREE_DELETE_BLOCKED_REASON}
            </p>
          ) : null}
        </DialogPanel>
        <DialogFooter>
          {completed ? (
            <Button type="button" onClick={close}>
              Close
            </Button>
          ) : step === "dirty" ? (
            <>
              <Button
                type="button"
                variant="outline"
                disabled={isRemoving}
                onClick={() => setStep("choices")}
              >
                Back
              </Button>
              <Button
                type="button"
                variant="destructive"
                disabled={isRemoving || deletionBlocked}
                aria-describedby={showDeletionBlockedReason ? deletionBlockedReasonId : undefined}
                onClick={confirmDirty}
              >
                Delete dirty worktree
              </Button>
            </>
          ) : step === "prune" && plan ? (
            <>
              <Button
                type="button"
                variant="outline"
                disabled={isRemoving}
                onClick={() => setStep("choices")}
              >
                Back
              </Button>
              <Button
                type="button"
                variant="destructive"
                disabled={isRemoving || deletionBlocked}
                aria-describedby={showDeletionBlockedReason ? deletionBlockedReasonId : undefined}
                onClick={() =>
                  void executeDestructiveRemoval(
                    plan.availability === "missing-registered"
                      ? "cleanup-stale-registration"
                      : "delete-git-worktree",
                    hasDirtyChanges,
                    true,
                  )
                }
              >
                Confirm repository-wide prune
              </Button>
            </>
          ) : (
            <>
              <Button type="button" variant="outline" disabled={isRemoving} onClick={close}>
                Cancel
              </Button>
              <Button
                type="button"
                variant="outline"
                disabled={isRemoving || removalUnavailable || target === null}
                onClick={() => void detach()}
              >
                Remove from BiBCode
              </Button>
              {destructiveEligible ? (
                <Button
                  type="button"
                  variant="destructive"
                  disabled={isRemoving || deletionBlocked}
                  aria-describedby={showDeletionBlockedReason ? deletionBlockedReasonId : undefined}
                  onClick={beginDestructiveRemoval}
                >
                  {destructiveLabel}
                </Button>
              ) : null}
            </>
          )}
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
