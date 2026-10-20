import { memo, useCallback } from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";

export interface GitManagerSwitchWithChangesDialogProps {
  readonly open: boolean;
  readonly branchName: string;
  readonly busy: boolean;
  readonly errorMessage?: string | null;
  readonly branchDisabledReason?: string | null;
  readonly stashDisabledReason?: string | null;
  readonly onOpenChange: (open: boolean) => void;
  readonly onResolve: (resolution: { readonly strategy: "stash" | "bring" }) => Promise<void>;
}

export const GitManagerSwitchWithChangesDialog = memo(function GitManagerSwitchWithChangesDialog({
  open,
  branchName,
  busy,
  errorMessage = null,
  branchDisabledReason = null,
  stashDisabledReason = null,
  onOpenChange,
  onResolve,
}: GitManagerSwitchWithChangesDialogProps) {
  const leaveChangesDisabledReason = branchDisabledReason ?? stashDisabledReason;
  const leaveChanges = useCallback(() => void onResolve({ strategy: "stash" }), [onResolve]);
  const bringChanges = useCallback(() => void onResolve({ strategy: "bring" }), [onResolve]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup>
        <DialogHeader>
          <DialogTitle>Switch to {branchName}?</DialogTitle>
          <DialogDescription>
            Choose what to do with the uncommitted changes in this checkout.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3 px-6 pb-4 text-sm">
          <p>
            <strong>Leave my changes</strong> saves them, new files included, in an ordinary,
            visible stash entry, then switches branches. The stash appears in the repository&apos;s
            normal stash list.
          </p>
          <p>
            <strong>Bring my changes</strong> switches branches while carrying the current
            working-tree changes across.
          </p>
          {errorMessage === null ? null : (
            <p role="alert" className="text-destructive">
              {errorMessage}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button
            aria-describedby={
              leaveChangesDisabledReason === null
                ? undefined
                : "git-manager-switch-stash-disabled-reason"
            }
            disabled={busy || leaveChangesDisabledReason !== null}
            title={leaveChangesDisabledReason ?? undefined}
            variant="outline"
            onClick={leaveChanges}
          >
            Leave my changes
          </Button>
          {leaveChangesDisabledReason === null ? null : (
            <span className="sr-only" id="git-manager-switch-stash-disabled-reason">
              {leaveChangesDisabledReason}
            </span>
          )}
          <Button
            aria-describedby={
              branchDisabledReason === null
                ? undefined
                : "git-manager-switch-branch-disabled-reason"
            }
            disabled={busy || branchDisabledReason !== null}
            title={branchDisabledReason ?? undefined}
            onClick={bringChanges}
          >
            Bring my changes
          </Button>
          {branchDisabledReason === null ? null : (
            <span className="sr-only" id="git-manager-switch-branch-disabled-reason">
              {branchDisabledReason}
            </span>
          )}
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
});
