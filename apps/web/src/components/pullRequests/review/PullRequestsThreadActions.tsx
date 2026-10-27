import type {
  PullRequestsPermissions,
  PullRequestsTimelineItem,
  ScopedProjectRef,
} from "@coddy/contracts";
import { useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { usePullRequestsActions } from "../usePullRequestsAction";
import { PullRequestsCommentBox } from "./PullRequestsCommentBox";
export function PullRequestsThreadActions({
  thread,
  permissions,
  projectRef,
  number,
  live = true,
}: {
  thread: Extract<PullRequestsTimelineItem, { kind: "thread" }>;
  permissions: PullRequestsPermissions;
  projectRef: ScopedProjectRef;
  number: number;
  /**
   * Resolving and replying only ever act on a live detail, never a
   * snapshot. Defaults to `true` so callers that have not adopted the split
   * keep acting as before.
   */
  live?: boolean;
}) {
  const { run, pending } = usePullRequestsActions();
  const draft = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, number).replyDrafts[thread.id] ?? "",
  );
  const [error, setError] = useState<string | null>(null);
  const resolvePermission = constrainPermission(
    permissions.resolveThreads,
    !live
      ? "Loading…"
      : !thread.canResolve
        ? "This thread cannot be resolved by your account"
        : pending
          ? "Wait for the current action to finish"
          : null,
  );
  return (
    <div className="space-y-3 p-3">
      <PermissionButton
        mutation
        permission={resolvePermission}
        variant="outline"
        size="sm"
        onClick={() => {
          setError(null);
          void run({
            action: "resolveThread",
            threadId: thread.id,
            resolved: !thread.isResolved,
          }).catch((cause) =>
            setError(
              cause instanceof Error ? cause.message : "Could not update the thread. Try again.",
            ),
          );
        }}
      >
        {thread.isResolved ? "Unresolve thread" : "Resolve thread"}
      </PermissionButton>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <PullRequestsCommentBox
        permission={constrainPermission(permissions.comment, !live ? "Loading…" : null)}
        busy={pending}
        label="Reply to thread"
        submitLabel="Reply"
        draft={draft}
        onDraftChange={(value) =>
          usePullRequestsStore.getState().setReplyDraft(projectRef, number, thread.id, value)
        }
        onSubmit={async (body) => {
          await run({ action: "replyThread", threadId: thread.id, body });
          const store = usePullRequestsStore.getState();
          if (store.selectDraft(projectRef, number).replyDrafts[thread.id] === body)
            store.setReplyDraft(projectRef, number, thread.id, null);
        }}
      />
    </div>
  );
}
