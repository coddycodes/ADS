import { useShallow } from "zustand/react/shallow";
import { randomUUID } from "../../../lib/utils";
import type {
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsTimelineItem,
  ScopedProjectRef,
} from "@coddy/contracts";
import type { DiffLineAnnotation } from "@pierre/diffs";
import { memo, useCallback, useMemo, type ReactNode } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import {
  PullRequestsFileDiff,
  type PullRequestsFileDiffProps,
} from "../detail/PullRequestsFileDiff";
import { PullRequestsTimelineItem as TimelineItem } from "../detail/PullRequestsTimelineItem";
import { createInlineCommentGutter } from "./inlineCommentGutter.logic";
import {
  PullRequestsInlineComposer,
  PullRequestsPendingComment,
} from "./PullRequestsInlineComposer";
type Thread = Extract<PullRequestsTimelineItem, { kind: "thread" }>;
const EMPTY_THREADS: readonly Thread[] = [];
export const PullRequestsReviewFile = memo(function PullRequestsReviewFile({
  detail,
  context,
  projectRef,
  threads = EMPTY_THREADS,
  live = true,
  ...diff
}: PullRequestsFileDiffProps & {
  detail: PullRequestsDetail;
  context: Extract<PullRequestsContext, { status: "available" }>;
  projectRef: ScopedProjectRef;
  threads?: readonly Thread[] | undefined;
  /**
   * Every review action rendered for this file (thread actions, pending
   * comments, the inline composer) only ever acts on a live detail, never
   * a snapshot. Defaults to `true` so callers that have not adopted the
   * split keep acting as before.
   */
  live?: boolean;
}) {
  const pending = usePullRequestsStore(
    useShallow((s) =>
      s
        .selectDraft(projectRef, detail.number)
        .pendingReview.filter((comment) => comment.path === diff.file.path),
    ),
  );
  const drafts = usePullRequestsStore((s) => s.selectDraft(projectRef, detail.number).inlineDrafts);
  const openComposer = useCallback(
    (path: string, startLine: number, line: number, side: "left" | "right") => {
      const gutter = createInlineCommentGutter();
      gutter.beginSelection(path, startLine, side);
      gutter.extendSelection(line);
      const selection = gutter.selectionRange();
      if (!selection) return;
      const store = usePullRequestsStore.getState();
      const current = store.selectDraft(projectRef, detail.number).inlineDrafts;
      const start = selection.startLine === selection.line ? null : selection.startLine;
      if (
        current.some(
          (c) =>
            c.path === path &&
            c.startLine === start &&
            c.line === selection.line &&
            c.side === side,
        )
      )
        return;
      // A drag may follow a click. Replace only empty composers; typed work stays anchored.
      for (const draft of current)
        if (draft.path === path && !draft.body)
          store.removeInlineDraft(projectRef, detail.number, draft.id);
      store.setInlineDraft(projectRef, detail.number, {
        ...selection,
        startLine: start,
        id: randomUUID(),
        body: "",
      });
    },
    [projectRef, detail.number],
  );
  const onLineClick = useCallback(
    (path: string, line: number, side: "left" | "right") => openComposer(path, line, line, side),
    [openComposer],
  );
  const annotations = useMemo(() => {
    const groups = new Map<
      string,
      { side: "additions" | "deletions"; lineNumber: number; nodes: ReactNode[] }
    >();
    const add = (line: number | null, side: "left" | "right", node: ReactNode) => {
      const lineNumber = line ?? 0;
      const key = `${side}:${lineNumber}`;
      let group = groups.get(key);
      if (!group) {
        group = { side: side === "left" ? "deletions" : "additions", lineNumber, nodes: [] };
        groups.set(key, group);
      }
      group.nodes.push(node);
    };
    for (const thread of threads)
      add(
        thread.line,
        thread.side,
        <TimelineItem
          key={thread.id}
          item={thread}
          permissions={detail.permissions}
          projectRef={projectRef}
          number={detail.number}
          context={context}
          url={detail.url}
          live={live}
        />,
      );
    for (const comment of pending)
      if (comment.path === diff.file.path)
        add(
          comment.line,
          comment.side,
          <PullRequestsPendingComment
            key={`pending:${comment.id}`}
            comment={comment}
            projectRef={projectRef}
            number={detail.number}
            permission={detail.permissions.review}
            live={live}
          />,
        );
    for (const draft of drafts)
      if (draft.path === diff.file.path)
        add(
          draft.line,
          draft.side,
          <PullRequestsInlineComposer
            key={`draft:${draft.id}`}
            draftId={draft.id}
            projectRef={projectRef}
            number={detail.number}
            permission={detail.permissions.review}
            headSha={detail.headSha}
            patch={diff.file.patch ?? ""}
            live={live}
          />,
        );
    return [...groups.values()].map(
      ({ side, lineNumber, nodes }): DiffLineAnnotation<ReactNode> => ({
        side,
        lineNumber,
        metadata: <div className="space-y-3 p-2 font-sans text-sm">{nodes}</div>,
      }),
    );
  }, [
    threads,
    pending,
    drafts,
    diff.file.path,
    diff.file.patch,
    detail.permissions,
    detail.number,
    detail.headSha,
    detail.url,
    projectRef,
    context,
    live,
  ]);
  return (
    <PullRequestsFileDiff
      {...diff}
      onLineClick={onLineClick}
      onLineRangeSelect={openComposer}
      annotations={annotations}
    />
  );
});
