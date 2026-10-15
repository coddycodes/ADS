import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import type { EnvironmentId, ScopedProjectRef } from "@bibcode/contracts";
import { memo, useCallback, useRef, useState } from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";
import { gitManagerEnvironment } from "~/state/gitManager";
import { useAtomCommand } from "~/state/use-atom-command";

import { resolvePartialDiscardDialogCopy } from "./GitManagerPartialDiscardDialog.logic";
import {
  type GitManagerLineSelection,
  resolveSelectionMutationFailure,
  toWireSelection,
} from "./gitManagerLineSelection";

export interface GitManagerPartialDiscardDialogProps {
  readonly open: boolean;
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly projectRef: ScopedProjectRef;
  readonly path: string;
  readonly generation: number;
  readonly selection: GitManagerLineSelection;
  readonly onOpenChange: (open: boolean) => void;
  readonly onCompleted: () => void;
  readonly onStale?: () => void;
}

export const GitManagerPartialDiscardDialog = memo(function GitManagerPartialDiscardDialog({
  open,
  scope,
  projectRef,
  path,
  generation,
  selection,
  onOpenChange,
  onCompleted,
  onStale,
}: GitManagerPartialDiscardDialogProps) {
  const { environmentId, cwd } = scope;
  const [isConfirming, setIsConfirming] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const confirmingRef = useRef(false);
  const discardPartial = useAtomCommand(gitManagerEnvironment.discardPartial, {
    reportFailure: false,
  });
  const copy = resolvePartialDiscardDialogCopy(selection, path);
  const cancel = useCallback(() => {
    if (!confirmingRef.current) onOpenChange(false);
  }, [onOpenChange]);
  const handleDialogOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (nextOpen || !confirmingRef.current) onOpenChange(nextOpen);
    },
    [onOpenChange],
  );
  const confirm = useCallback(async () => {
    if (confirmingRef.current) return;
    const snapshot = selection;
    const wire = toWireSelection(snapshot, path, generation);
    const [firstLine, ...remainingLines] = wire.selectedLines;
    if (firstLine === undefined) return;
    confirmingRef.current = true;
    setIsConfirming(true);
    setErrorMessage(null);
    try {
      const result = await discardPartial({
        environmentId,
        input: {
          cwd,
          projectId: projectRef.projectId,
          path: wire.path,
          selectedLines: [firstLine, ...remainingLines],
          baseGeneration: wire.baseGeneration,
        },
      });
      if (result._tag === "Failure") {
        const resolution = resolveSelectionMutationFailure(
          snapshot,
          squashAtomCommandFailure(result),
        );
        setErrorMessage(resolution.message);
        if (resolution.stale) onStale?.();
        return;
      }
      onCompleted();
      onOpenChange(false);
    } catch (error) {
      setErrorMessage(resolveSelectionMutationFailure(snapshot, error).message);
    } finally {
      confirmingRef.current = false;
      setIsConfirming(false);
    }
  }, [
    cwd,
    discardPartial,
    environmentId,
    generation,
    onCompleted,
    onOpenChange,
    onStale,
    path,
    projectRef.projectId,
    selection,
  ]);

  return (
    <Dialog open={open} onOpenChange={handleDialogOpenChange}>
      <DialogPopup>
        <DialogHeader>
          <DialogTitle>{copy.title}</DialogTitle>
          <DialogDescription>{copy.description}</DialogDescription>
        </DialogHeader>
        {errorMessage === null ? null : (
          <p aria-live="polite" className="px-6 pb-2 text-sm text-destructive">
            {errorMessage}
          </p>
        )}
        <DialogFooter>
          <Button autoFocus disabled={isConfirming} variant="outline" onClick={cancel}>
            Cancel
          </Button>
          <Button
            disabled={isConfirming || selection.type === "none"}
            variant="destructive"
            onClick={() => void confirm()}
          >
            {isConfirming ? "Discarding…" : copy.confirmLabel}
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
});
