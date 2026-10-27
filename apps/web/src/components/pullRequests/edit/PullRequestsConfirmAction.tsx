import type { PullRequestsPermission } from "@coddy/contracts";
import { useRef, useState, type ReactNode } from "react";
import {
  AlertDialog,
  AlertDialogPopup,
  AlertDialogHeader,
  AlertDialogFooter,
  AlertDialogTitle,
  AlertDialogDescription,
} from "../../ui/alert-dialog";
import { Button } from "../../ui/button";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { pullRequestsActionError, usePullRequestsActions } from "../usePullRequestsAction";

export function PullRequestsConfirmAction({
  title,
  children,
  confirmLabel,
  permission,
  onConfirm,
  onClose,
  destructive = false,
}: {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  permission: PullRequestsPermission;
  onConfirm: () => Promise<unknown>;
  onClose: () => void;
  destructive?: boolean;
}) {
  const { requestKind } = usePullRequestsActions();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  async function confirm() {
    if (!permission.allowed || busy.current) return;
    busy.current = true;
    setPending(true);
    setError(null);
    try {
      await onConfirm();
      onClose();
    } catch (cause) {
      setError(pullRequestsActionError(cause, requestKind));
    } finally {
      busy.current = false;
      setPending(false);
    }
  }
  return (
    <AlertDialog
      open
      onOpenChange={(open) => {
        if (!open && !busy.current) onClose();
      }}
    >
      <AlertDialogPopup data-text-surface="popover">
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription render={<div />}>{children}</AlertDialogDescription>
        </AlertDialogHeader>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <AlertDialogFooter>
          <Button variant="outline" disabled={pending} onClick={onClose}>
            Cancel
          </Button>
          <PermissionButton
            mutation
            permission={constrainPermission(
              permission,
              pending ? "Wait for the current action to finish" : null,
            )}
            variant={destructive ? "destructive" : "default"}
            onClick={() => void confirm()}
          >
            {pending ? "Working…" : confirmLabel}
          </PermissionButton>
        </AlertDialogFooter>
      </AlertDialogPopup>
    </AlertDialog>
  );
}
