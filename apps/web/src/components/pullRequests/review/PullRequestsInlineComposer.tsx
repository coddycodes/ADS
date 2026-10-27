import { reconcilePendingReview } from "./pendingReview.logic";
import type { PullRequestsPermission, ScopedProjectRef } from "@coddy/contracts";
import { useId, useMemo, useRef, useState } from "react";
import { usePullRequestsStore, type PendingInlineComment } from "../../../pullRequestsStore";
import { Button } from "../../ui/button";
import { PullRequestsMarkdown } from "../shared/PullRequestsMarkdown";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { usePullRequestsActions } from "../usePullRequestsAction";
import { insertSuggestion, sourceLinesForSelection } from "./inlineCommentGutter.logic";
export function PullRequestsInlineComposer({
  projectRef,
  number,
  draftId,
  permission,
  headSha,
  patch,
  live = true,
}: {
  projectRef: ScopedProjectRef;
  number: number;
  draftId: string;
  permission: PullRequestsPermission;
  headSha: string;
  patch: string;
  /**
   * Adding or submitting a review comment only ever acts on a live detail,
   * never a snapshot. Defaults to `true` so callers that have not adopted
   * the split keep acting as before.
   */
  live?: boolean;
}) {
  const draft = usePullRequestsStore((s) =>
    s.selectDraft(projectRef, number).inlineDrafts.find((c) => c.id === draftId),
  );
  const { run, pending } = usePullRequestsActions();
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const busy = useRef(false);
  const id = useId();
  const line = draft?.line;
  const startLine = draft?.startLine;
  const side = draft?.side;
  const source = useMemo(
    () =>
      line !== undefined && side
        ? sourceLinesForSelection(patch, { line, startLine: startLine ?? line, side })
        : null,
    [patch, line, startLine, side],
  );
  if (!draft) return null;
  const available = constrainPermission(
    permission,
    !live
      ? "Loading…"
      : pending || sending
        ? "Wait for the current action to finish"
        : !draft.body.trim()
          ? "Write a review comment first"
          : null,
  );
  const discard = () =>
    usePullRequestsStore.getState().removeInlineDraft(projectRef, number, draft.id);
  const addPending = () => {
    if (!available.allowed) return;
    const store = usePullRequestsStore.getState();
    const comments = store.selectDraft(projectRef, number).pendingReview;
    store.setPendingReview(
      projectRef,
      number,
      comments.some((c) => c.id === draft.id)
        ? comments.map((c) => (c.id === draft.id ? draft : c))
        : [...comments, draft],
    );
    discard();
  };
  async function addSingle() {
    if (!draft || !available.allowed || busy.current) return;
    busy.current = true;
    setSending(true);
    setError(null);
    const { id: _id, ...comment } = draft;
    const originalPending = usePullRequestsStore
      .getState()
      .selectDraft(projectRef, number)
      .pendingReview.find((c) => c.id === draft.id);
    try {
      const result = await run({
        action: "submitReview",
        event: "comment",
        body: null,
        headSha,
        comments: [comment],
      });
      if (result.kind !== "reviewSubmitted")
        throw new Error("The host did not confirm the comment. Refresh before trying again.");
      if (result.failed.length > 0)
        throw new Error(result.failed.map((failure) => failure.message).join("\n"));
      const store = usePullRequestsStore.getState();
      if (
        store.selectDraft(projectRef, number).inlineDrafts.find((c) => c.id === draft.id)?.body ===
        draft.body
      )
        discard();
      store.setPendingReview(
        projectRef,
        number,
        reconcilePendingReview(
          store.selectDraft(projectRef, number).pendingReview,
          originalPending ? [originalPending] : [],
          result,
        ),
      );
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not post the comment. Try again.");
    } finally {
      busy.current = false;
      setSending(false);
    }
  }
  return (
    <section
      aria-label="Inline review comment"
      className="space-y-3 border border-border bg-background p-3 text-foreground"
      data-text-surface="background"
    >
      <label htmlFor={id} className="block text-sm font-medium">
        Comment on {draft.path}:
        {draft.startLine !== null && draft.startLine !== draft.line ? `${draft.startLine}–` : ""}
        {draft.line}
      </label>
      <textarea
        id={id}
        value={draft.body}
        disabled={!permission.allowed}
        readOnly={pending || sending}
        className="min-h-24 w-full rounded-md border border-border bg-background p-3 text-sm"
        onChange={(event) =>
          usePullRequestsStore
            .getState()
            .setInlineDraft(projectRef, number, { ...draft, body: event.currentTarget.value })
        }
        onKeyDown={(event) => {
          if (
            event.key === "Enter" &&
            (event.ctrlKey || event.metaKey) &&
            !event.nativeEvent.isComposing
          ) {
            event.preventDefault();
            addPending();
          }
        }}
      />
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <PermissionButton
          permission={constrainPermission(
            permission,
            !live
              ? "Loading…"
              : pending || sending
                ? "Wait for the current action to finish"
                : source === null
                  ? "Source lines are not available in this patch"
                  : draft.side === "left"
                    ? "Suggestions apply to the new version of the file"
                    : null,
          )}
          variant="outline"
          size="sm"
          onClick={() => {
            if (source !== null)
              usePullRequestsStore.getState().setInlineDraft(projectRef, number, {
                ...draft,
                body: insertSuggestion(draft.body, source),
              });
          }}
        >
          Insert suggestion
        </PermissionButton>
        <PermissionButton permission={available} onClick={addPending} size="sm">
          Add review comment
        </PermissionButton>
        <PermissionButton
          mutation
          permission={available}
          variant="outline"
          size="sm"
          onClick={() => void addSingle()}
        >
          {sending ? "Posting…" : "Add single comment"}
        </PermissionButton>
        <Button variant="ghost" size="sm" disabled={pending || sending} onClick={discard}>
          Discard draft
        </Button>
      </div>
    </section>
  );
}
export function PullRequestsPendingComment({
  comment,
  projectRef,
  number,
  permission,
  live = true,
}: {
  comment: PendingInlineComment;
  projectRef: ScopedProjectRef;
  number: number;
  permission: PullRequestsPermission;
  /**
   * Editing a pending review comment only ever acts on a live detail, never
   * a snapshot. Defaults to `true` so callers that have not adopted the
   * split keep acting as before.
   */
  live?: boolean;
}) {
  const { pending } = usePullRequestsActions();
  return (
    <article
      className="space-y-2 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-foreground"
      data-text-surface="background"
    >
      <p className="text-sm font-medium">Pending review comment</p>
      <PullRequestsMarkdown text={comment.body} />
      <div className="flex gap-2">
        <PermissionButton
          permission={constrainPermission(
            permission,
            !live ? "Loading…" : pending ? "Wait for the current action to finish" : null,
          )}
          variant="outline"
          size="sm"
          onClick={() => {
            const store = usePullRequestsStore.getState();
            if (
              !store.selectDraft(projectRef, number).inlineDrafts.some((c) => c.id === comment.id)
            )
              store.setInlineDraft(projectRef, number, comment);
          }}
        >
          Edit
        </PermissionButton>
        <Button
          variant="ghost"
          size="sm"
          disabled={pending}
          onClick={() => {
            const store = usePullRequestsStore.getState();
            store.setPendingReview(
              projectRef,
              number,
              store
                .selectDraft(projectRef, number)
                .pendingReview.filter((c) => c.id !== comment.id),
            );
            store.removeInlineDraft(projectRef, number, comment.id);
          }}
        >
          Remove
        </Button>
      </div>
    </article>
  );
}
