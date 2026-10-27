import { PullRequestsBodyEditor } from "../edit/PullRequestsBodyEditor";
import { PullRequestsPendingReviewBar } from "../review/PullRequestsPendingReviewBar";
import type { PullRequestsScope } from "../usePullRequestsAction";
import { usePullRequestsActions } from "../usePullRequestsAction";
import { PullRequestsConversationComment } from "../review/PullRequestsCommentBox";
import type {
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsTimeline,
  ScopedProjectRef,
} from "@coddy/contracts";
import { LegendList } from "@legendapp/list/react";
import { memo, useCallback, useMemo } from "react";
import { formatRelativeTimeLabel } from "../../../timestampFormat";
import { PullRequestsActor } from "../shared/PullRequestsActor";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { PullRequestsReactions } from "../shared/PullRequestsReactions";
import { groupTimeline } from "./pullRequestsDetail.logic";
import { PullRequestsTimelineItem } from "./PullRequestsTimelineItem";
import { PullRequestsSideColumn } from "./PullRequestsSideColumn";
import { PullRequestsApproveBar } from "./PullRequestsApproveBar";
import { PullRequestsMergeBox } from "./PullRequestsMergeBox";
export interface PullRequestsConversationProps {
  detail: PullRequestsDetail;
  context: Extract<PullRequestsContext, { status: "available" }>;
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
  timeline: PullRequestsTimeline;
  detailsRefreshing?: boolean;
  /**
   * Action controls (review, merge, side column pickers, timeline items,
   * reactions, comment box) only act on a live detail that succeeded on
   * this mount, never on a painted snapshot. Those controls still render
   * from `detail` but stay inactive with a "Loading…" reason until this
   * exists. Omitting this falls back to `detail`, so callers that have not
   * adopted the split keep acting as before.
   */
  liveDetail?: PullRequestsDetail | null;
}
const timelineKey = (item: PullRequestsTimeline["items"][number]) => item.id;
export const PullRequestsConversation = memo(function PullRequestsConversation({
  detail,
  context,
  projectRef,
  scope,
  timeline,
  detailsRefreshing = false,
  liveDetail = detail,
}: PullRequestsConversationProps) {
  const { run, pending } = usePullRequestsActions();
  const items = useMemo(() => groupTimeline(timeline.items), [timeline.items]);
  // React, comment, reply, resolve, dismiss, edit, delete, minimize, and
  // apply-suggestions all only ever act on a live detail, never a snapshot.
  const live = liveDetail !== null;
  const renderItem = useCallback(
    ({ item }: { item: PullRequestsTimeline["items"][number] }) => (
      <PullRequestsTimelineItem
        item={item}
        permissions={detail.permissions}
        projectRef={projectRef}
        number={detail.number}
        context={context}
        url={detail.url}
        live={live}
      />
    ),
    [context, detail.number, detail.permissions, detail.url, projectRef, live],
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PullRequestsPendingReviewBar
        detail={detail}
        live={liveDetail}
        context={context}
        projectRef={projectRef}
      />
      <LegendList
        data={items}
        keyExtractor={timelineKey}
        renderItem={renderItem}
        extraData={renderItem}
        recycleItems={false}
        estimatedItemSize={220}
        className="min-h-0 flex-1"
        role="list"
        aria-label="Conversation"
        data-text-surface="background"
        ListHeaderComponent={
          <>
            <article className="m-3 space-y-3 rounded-lg border border-border p-3">
              <header className="flex flex-wrap items-center justify-between gap-2 text-xs">
                <div className="flex items-center gap-2">
                  <PullRequestsActor actor={detail.author} />
                  <time
                    dateTime={detail.createdAt}
                    title={detail.createdAt}
                    className="text-muted-foreground"
                  >
                    {formatRelativeTimeLabel(detail.createdAt)}
                  </time>
                </div>
              </header>
              <PullRequestsBodyEditor
                detail={detail}
                live={liveDetail}
                projectRef={projectRef}
                baseUrl={`${context.webUrl}/`}
              />
              <PullRequestsReactions
                reactions={detail.reactions}
                permission={detail.permissions.react}
                busy={pending}
                live={live}
                onToggle={(content, on) => run({ action: "react", targetId: null, content, on })}
              />
            </article>
            <PullRequestsApproveBar detail={detail} live={liveDetail} context={context} />
          </>
        }
        ListFooterComponent={
          <div className="space-y-4 p-3">
            {timeline.truncated ? (
              <p className="text-sm">
                <PullRequestsExternalLink href={detail.url}>
                  Older activity is on the host page
                </PullRequestsExternalLink>
              </p>
            ) : null}
            <PullRequestsConversationComment
              permission={detail.permissions.comment}
              projectRef={projectRef}
              number={detail.number}
              live={live}
            />
            <div data-pull-request-side-column="inline" className="@3xl/pull-request:hidden">
              <PullRequestsSideColumn
                detail={detail}
                live={liveDetail}
                context={context}
                scope={scope}
                projectRef={projectRef}
                detailsRefreshing={detailsRefreshing}
              />
            </div>
            <PullRequestsMergeBox
              detail={detail}
              live={liveDetail}
              context={context}
              projectRef={projectRef}
            />
          </div>
        }
      />
    </div>
  );
});
