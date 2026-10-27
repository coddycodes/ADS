import type { PullRequestsPermission, PullRequestsTimelineItem } from "@coddy/contracts";
import { useId, useRef, useState } from "react";
import {
  AlertDialog,
  AlertDialogPopup,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogHeader,
  AlertDialogFooter,
} from "../../ui/alert-dialog";
import { Button } from "../../ui/button";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { usePullRequestsActions } from "../usePullRequestsAction";
export function PullRequestsDismissReview({
  review,
  permission,
  host,
  live = true,
}: {
  review: Extract<PullRequestsTimelineItem, { kind: "review" }>;
  permission: PullRequestsPermission;
  host: string;
  /**
   * Dismissing a review only ever acts on a live detail, never a snapshot.
   * Defaults to `true` so callers that have not adopted the split keep
   * acting as before.
   */
  live?: boolean;
}) {
  const { run, pending } = usePullRequestsActions();
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [dismissing, setDismissing] = useState(false);
  const busy = useRef(false);
  const id = useId();
  const available = constrainPermission(
    permission,
    !live
      ? "Loading…"
      : !review.canDismiss
        ? "This review cannot be dismissed"
        : pending || dismissing
          ? "Wait for the current action to finish"
          : null,
  );
  async function dismiss() {
    if (!available.allowed || !message.trim() || busy.current) return;
    busy.current = true;
    setDismissing(true);
    setError(null);
    try {
      await run({ action: "dismissReview", reviewId: review.id, message });
      setOpen(false);
      setMessage("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not dismiss the review. Try again.");
    } finally {
      busy.current = false;
      setDismissing(false);
    }
  }
  return (
    <>
      <PermissionButton
        mutation
        permission={available}
        variant="outline"
        size="sm"
        onClick={() => setOpen(true)}
      >
        Dismiss review
      </PermissionButton>
      <AlertDialog
        open={open}
        onOpenChange={(value) => {
          if (!dismissing) setOpen(value);
        }}
      >
        <AlertDialogPopup data-text-surface="popover">
          <AlertDialogHeader>
            <AlertDialogTitle>Dismiss review</AlertDialogTitle>
            <AlertDialogDescription>
              This dismisses {review.author.login}’s review on {host}. Include a message explaining
              why.
            </AlertDialogDescription>
            <label htmlFor={id} className="text-sm font-medium">
              Message (required)
            </label>
            <textarea
              id={id}
              required
              value={message}
              readOnly={pending || dismissing}
              onChange={(event) => setMessage(event.currentTarget.value)}
              className="min-h-24 w-full rounded-md border border-border bg-background p-3 text-sm"
            />
            {error ? (
              <p role="alert" className="text-sm text-destructive">
                {error}
              </p>
            ) : null}
          </AlertDialogHeader>
          <AlertDialogFooter>
            <Button variant="outline" disabled={dismissing} onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <PermissionButton
              mutation
              permission={constrainPermission(
                available,
                !message.trim() ? "Enter a message explaining the dismissal" : null,
              )}
              variant="destructive"
              onClick={() => void dismiss()}
            >
              {dismissing ? "Dismissing…" : "Dismiss review"}
            </PermissionButton>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </>
  );
}
