import type { PullRequestsDetail } from "@coddy/contracts";
import { useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { Popover, PopoverTrigger } from "../../ui/popover";
import {
  PullRequestsReviewPopover,
  type PullRequestsReviewProps,
} from "./PullRequestsReviewPopover";
export function PullRequestsPendingReviewBar({
  detail,
  live = detail,
  context,
  projectRef,
}: PullRequestsReviewProps & {
  /**
   * Submitting a review only ever acts on a live detail; a snapshot keeps the
   * trigger inactive. Omitting this falls back to `detail`, so callers that
   * have not adopted the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
}) {
  const count = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, detail.number).pendingReview.length,
  );
  const [open, setOpen] = useState(false);
  const permission = constrainPermission(
    { allowed: true, reason: null },
    live === null ? "Loading…" : null,
  );
  return (
    <div
      className="sticky top-0 z-10 shrink-0 border-b border-panel-separator bg-background px-3 py-2"
      data-text-surface="background"
    >
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          disabled={!permission.allowed}
          render={
            <PermissionButton
              permission={permission}
              variant={count > 0 ? "default" : "outline"}
              size="sm"
            />
          }
        >
          Review · {count} pending comments
        </PopoverTrigger>
        {open && live ? (
          <PullRequestsReviewPopover
            detail={live}
            context={context}
            projectRef={projectRef}
            onClose={() => setOpen(false)}
          />
        ) : null}
      </Popover>
    </div>
  );
}
