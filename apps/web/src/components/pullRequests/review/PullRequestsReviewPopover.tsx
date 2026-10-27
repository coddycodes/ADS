import type { PullRequestsContext, PullRequestsDetail, ScopedProjectRef } from "@coddy/contracts";
import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { useRef, useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { Button } from "../../ui/button";
import { PopoverPopup, PopoverTitle } from "../../ui/popover";
import { toastManager } from "../../ui/toast";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { usePullRequestsActions } from "../usePullRequestsAction";
import { reconcilePendingReview, reviewComments } from "./pendingReview.logic";
export interface PullRequestsReviewProps {
  detail: PullRequestsDetail;
  context: Extract<PullRequestsContext, { status: "available" }>;
  projectRef: ScopedProjectRef;
}
export function PullRequestsReviewPopover({
  detail,
  context,
  projectRef,
  onClose,
}: PullRequestsReviewProps & { onClose: () => void }) {
  const { run, pending } = usePullRequestsActions();
  const comments = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, detail.number).pendingReview,
  );
  const body = usePullRequestsStore((s) => s.selectDraft(projectRef, detail.number).reviewBody);
  const [event, setEvent] = useState<"comment" | "approve" | "request_changes">("comment");
  const [submitting, setSubmitting] = useState(false);
  const busy = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const vocabulary = context.capabilities.vocabulary;
  const options = [
    { value: "comment", label: "Comment", permission: detail.permissions.review },
    { value: "approve", label: vocabulary.approve, permission: detail.permissions.approve },
    {
      value: "request_changes",
      label: vocabulary.requestChanges,
      permission: detail.permissions.requestChanges,
    },
  ] as const;
  const selection = options.find((option) => option.value === event)!;
  const disabledReason =
    pending || submitting
      ? "Wait for the current action to finish"
      : event === "comment" && !body.trim() && comments.length === 0
        ? "Write a summary or add a review comment first"
        : null;
  const submitPermission = constrainPermission(selection.permission, disabledReason);
  async function submit() {
    if (!submitPermission.allowed || busy.current) return;
    busy.current = true;
    setSubmitting(true);
    setError(null);
    const snapshot = [...comments];
    try {
      const result = await run({
        action: "submitReview",
        event,
        body: body.trim() ? body : null,
        headSha: detail.headSha,
        comments: reviewComments(snapshot),
      });
      if (result.kind !== "reviewSubmitted")
        throw new Error("The host did not confirm the review. Refresh before trying again.");
      const store = usePullRequestsStore.getState();
      store.setPendingReview(
        projectRef,
        detail.number,
        reconcilePendingReview(
          store.selectDraft(projectRef, detail.number).pendingReview,
          snapshot,
          result,
        ),
      );
      if (result.reviewPosted && store.selectDraft(projectRef, detail.number).reviewBody === body)
        store.setReviewBody(projectRef, detail.number, "");
      if (result.reviewPosted && result.failed.length > 0) setEvent("comment");
      if (result.failed.length === 0) {
        toastManager.add({ type: "success", title: "Review submitted" });
        onClose();
      } else {
        const title = `${result.failed.length} of ${snapshot.length} comments could not be posted`;
        const description = result.failed
          .map((failure) => `${failure.path}:${failure.line}: ${failure.message}`)
          .join("\n");
        setError(`${title}\n${description}`);
        toastManager.add({ type: "error", title, description });
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not submit the review. Try again.");
    } finally {
      busy.current = false;
      setSubmitting(false);
    }
  }
  async function runSeparate(action: "revokeApproval" | "removeOwnChangeRequest") {
    setError(null);
    try {
      await run({ action });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not update your review. Try again.");
    }
  }
  return (
    <PopoverPopup
      align="start"
      className="w-[min(28rem,calc(100vw-2rem))]"
      data-text-surface="popover"
    >
      <div className="space-y-4">
        <PopoverTitle>Submit review</PopoverTitle>
        <label className="block space-y-2 text-sm">
          <span>Review summary</span>
          <textarea
            aria-label="Review summary"
            value={body}
            readOnly={pending || submitting}
            className="min-h-28 w-full rounded-lg border border-border bg-background p-3 text-sm"
            onChange={(e) =>
              usePullRequestsStore
                .getState()
                .setReviewBody(projectRef, detail.number, e.currentTarget.value)
            }
          />
        </label>
        <RadioGroup
          aria-label="Review type"
          value={event}
          onValueChange={(value) => {
            if (value === "comment" || value === "approve" || value === "request_changes")
              setEvent(value);
          }}
          className="flex flex-col gap-2"
        >
          {options.map((option) => {
            const permission = constrainPermission(
              option.permission,
              pending || submitting ? "Wait for the current action to finish" : null,
            );
            return (
              <Radio.Root
                nativeButton
                key={option.value}
                value={option.value}
                disabled={!permission.allowed}
                render={
                  <PermissionButton
                    permission={permission}
                    variant="outline"
                    className="w-full justify-start gap-2"
                  />
                }
              >
                <span
                  aria-hidden="true"
                  className="flex size-4 items-center justify-center rounded-full border border-current"
                >
                  <Radio.Indicator className="size-2 rounded-full bg-current" />
                </span>
                {option.label}
              </Radio.Root>
            );
          })}
        </RadioGroup>
        {error ? (
          <p role="alert" className="whitespace-pre-wrap text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <div className="flex flex-wrap gap-2 border-t border-border pt-3">
          <PermissionButton
            mutation
            permission={constrainPermission(
              detail.permissions.revokeApproval,
              pending || submitting ? "Wait for the current action to finish" : null,
            )}
            variant="outline"
            size="sm"
            onClick={() => void runSeparate("revokeApproval")}
          >
            Revoke approval
          </PermissionButton>
          <PermissionButton
            mutation
            permission={constrainPermission(
              detail.permissions.removeOwnChangeRequest,
              pending || submitting ? "Wait for the current action to finish" : null,
            )}
            variant="outline"
            size="sm"
            onClick={() => void runSeparate("removeOwnChangeRequest")}
          >
            Remove my change request
          </PermissionButton>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="outline" onClick={onClose}>
            Close review
          </Button>
          <PermissionButton mutation permission={submitPermission} onClick={() => void submit()}>
            {submitting ? "Submitting…" : "Submit review"}
          </PermissionButton>
        </div>
      </div>
    </PopoverPopup>
  );
}
