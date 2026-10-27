import type { PullRequestsActionResult } from "@coddy/contracts";
import type { PendingInlineComment } from "../../../pullRequestsStore";
export function reviewComments(comments: readonly PendingInlineComment[]) {
  return comments.map(({ path, line, startLine, side, body }) => ({
    path,
    line,
    startLine,
    side,
    body,
  }));
}
export function reconcilePendingReview(
  current: readonly PendingInlineComment[],
  submitted: readonly PendingInlineComment[],
  result: Extract<PullRequestsActionResult, { kind: "reviewSubmitted" }>,
): PendingInlineComment[] {
  const sent = new Map(submitted.map((comment) => [comment.id, comment]));
  // Match the host receipt to the submitted body; preserve concurrent local edits.
  const failed = new Set(
    result.failed.map((comment) => JSON.stringify([comment.path, comment.line, comment.body])),
  );
  return current.filter((comment) => {
    const original = sent.get(comment.id);
    if (
      !original ||
      original.body !== comment.body ||
      original.path !== comment.path ||
      original.line !== comment.line ||
      original.side !== comment.side ||
      original.startLine !== comment.startLine
    )
      return true;
    return failed.has(JSON.stringify([comment.path, comment.line, comment.body]));
  });
}
