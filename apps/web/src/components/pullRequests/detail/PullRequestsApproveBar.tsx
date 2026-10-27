import type { PullRequestsContext, PullRequestsDetail } from "@coddy/contracts";
import { memo, useRef, useState } from "react";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { toastManager } from "../../ui/toast";
import { pullRequestsActionError, usePullRequestsActions } from "../usePullRequestsAction";
import { approvalStatusNote } from "./pullRequestsDetail.logic";

/**
 * GitLab's overview Approve control. One click approves the current head.
 * Pending inline comments stay in the review draft; they are submitted only
 * from the review popover.
 */
export const PullRequestsApproveBar = memo(function PullRequestsApproveBar({
  detail,
  live = detail,
  context,
}: {
  detail: PullRequestsDetail;
  /**
   * Approve and revoke act only on this live detail. Until it exists the
   * control stays inactive with "Loading…", even when a snapshot would allow
   * the action. Omitting this falls back to `detail`.
   */
  live?: PullRequestsDetail | null;
  context: Extract<PullRequestsContext, { status: "available" }>;
}) {
  const { run, pending, requestKind } = usePullRequestsActions();
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const busy = useRef(false);
  const visible = context.provider === "gitlab" && detail.state === "open";
  const revoking = live?.permissions.revokeApproval.allowed === true;
  const permission =
    live === null
      ? { allowed: false, reason: "Loading…" }
      : constrainPermission(
          revoking ? live.permissions.revokeApproval : live.permissions.approve,
          pending || submitting ? "Wait for the current action to finish" : null,
        );
  const label = revoking ? "Revoke approval" : context.capabilities.vocabulary.approve;
  async function submit() {
    if (!permission.allowed || busy.current || live === null) return;
    busy.current = true;
    setSubmitting(true);
    setError(null);
    try {
      if (revoking) {
        await run({ action: "revokeApproval" });
        toastManager.add({ type: "success", title: "Approval revoked" });
      } else {
        const result = await run({
          action: "submitReview",
          event: "approve",
          body: null,
          headSha: live.headSha,
          comments: [],
        });
        if (result.kind !== "reviewSubmitted" || !result.reviewPosted)
          throw new Error("The host did not confirm the approval. Refresh before trying again.");
        toastManager.add({ type: "success", title: "Approved" });
      }
    } catch (cause) {
      setError(pullRequestsActionError(cause, requestKind));
    } finally {
      busy.current = false;
      setSubmitting(false);
    }
  }
  if (!visible) return null;
  return (
    <div
      aria-label="Approval"
      className="mx-3 mb-3 flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-border bg-muted/40 px-3 py-2"
    >
      <PermissionButton mutation permission={permission} onClick={() => void submit()}>
        {submitting ? (revoking ? "Revoking…" : "Approving…") : label}
      </PermissionButton>
      <p className="text-sm text-muted-foreground">
        {approvalStatusNote(detail.readiness.requiredApprovals)}
      </p>
      {error ? (
        <p role="alert" className="basis-full text-sm text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
});
