import { PullRequestsDismissReview } from "../review/PullRequestsDismissReview";
import { PullRequestsThreadActions } from "../review/PullRequestsThreadActions";
import { PullRequestsCommentActions } from "../review/PullRequestsCommentActions";
import { usePullRequestsActions } from "../usePullRequestsAction";
import type {
  PullRequestsContext,
  PullRequestsPermissions,
  PullRequestsTimelineItem as TimelineItem,
  ScopedProjectRef,
} from "@bibcode/contracts";
import { CheckIcon, CircleDotIcon, MessageSquareIcon, XIcon } from "lucide-react";
import { memo, type ReactNode, useState } from "react";
import { formatRelativeTimeLabel } from "../../../timestampFormat";
import { Button } from "../../ui/button";
import { PullRequestsActor } from "../shared/PullRequestsActor";
import { PullRequestsMarkdown } from "../shared/PullRequestsMarkdown";
import { PullRequestsReactions } from "../shared/PullRequestsReactions";
import { PullRequestsSuggestionBlock } from "../shared/PullRequestsSuggestionBlock";
import { timelineEventText } from "./pullRequestsDetail.logic";
import { usePullRequestsNavigation } from "./pullRequestsNavigation";
export interface PullRequestsTimelineItemProps {
  item: TimelineItem;
  permissions: PullRequestsPermissions;
  projectRef: ScopedProjectRef;
  number: number;
  context: Extract<PullRequestsContext, { status: "available" }>;
  url: string;
  /**
   * React, comment, reply, resolve, dismiss, edit, delete, minimize, and
   * apply-suggestions all only ever act on a live detail, never a snapshot.
   * Defaults to `true` so callers that have not adopted the split keep
   * acting as before.
   */
  live?: boolean;
}
const REVIEW_LABELS = {
  approved: "approved these changes",
  changes_requested: "requested changes",
  commented: "commented",
  dismissed: "review dismissed",
  pending: "review pending",
};
function CommentBody({
  comment,
  minimized = false,
  baseUrl,
  permissions,
  projectRef,
  number,
  context,
  url,
  live = true,
}: Pick<
  PullRequestsTimelineItemProps,
  "permissions" | "projectRef" | "number" | "context" | "url" | "live"
> & {
  comment:
    | Extract<TimelineItem, { kind: "comment" }>
    | Extract<TimelineItem, { kind: "thread" }>["comments"][number];
  minimized?: boolean;
  baseUrl: string;
}) {
  const { run, pending } = usePullRequestsActions();
  const [shown, setShown] = useState(false);
  return (
    <article className="space-y-3 p-3">
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <PullRequestsActor actor={comment.author} />
        <time
          className="text-muted-foreground"
          dateTime={comment.createdAt}
          title={comment.createdAt}
        >
          {formatRelativeTimeLabel(comment.createdAt)}
        </time>
        {comment.updatedAt !== comment.createdAt ? (
          <span className="text-muted-foreground">edited</span>
        ) : null}
      </div>
      {minimized && !shown ? (
        <Button variant="outline" size="sm" onClick={() => setShown(true)}>
          Show comment
        </Button>
      ) : (
        <>
          <PullRequestsMarkdown text={comment.body} baseUrl={baseUrl} />
          {"suggestion" in comment && comment.suggestion ? (
            <PullRequestsSuggestionBlock
              suggestion={comment.suggestion}
              permission={permissions.applySuggestion}
              live={live}
            />
          ) : null}
          <PullRequestsReactions
            reactions={comment.reactions}
            permission={permissions.react}
            busy={pending}
            live={live}
            onToggle={(content, on) => run({ action: "react", targetId: comment.id, content, on })}
          />
        </>
      )}
      <PullRequestsCommentActions
        comment={comment}
        permissions={permissions}
        projectRef={projectRef}
        number={number}
        host={context.host}
        url={url}
        live={live}
      />
    </article>
  );
}
/** The conversation's link to a file opens the Changes tab at that file, wherever the view is. */
function PullRequestsFileLink({ path, children }: { path: string; children: ReactNode }) {
  const navigation = usePullRequestsNavigation();
  return (
    <button
      type="button"
      className="break-all text-left font-mono text-primary underline underline-offset-4"
      onClick={() => navigation.showFile(path)}
    >
      {children}
    </button>
  );
}
export const PullRequestsTimelineItem = memo(function PullRequestsTimelineItem({
  item,
  permissions,
  projectRef,
  number,
  context,
  url,
  live = true,
}: PullRequestsTimelineItemProps) {
  if (item.kind === "event")
    return (
      <div
        role="listitem"
        data-text-surface="background"
        className="flex items-start gap-2 px-3 py-4 text-sm"
      >
        <CircleDotIcon
          aria-hidden="true"
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
        />
        <p>
          {item.actor ? (
            <>
              <PullRequestsActor actor={item.actor} />{" "}
            </>
          ) : null}
          {timelineEventText(item)}{" "}
          <time
            dateTime={item.createdAt}
            title={item.createdAt}
            className="text-xs text-muted-foreground"
          >
            {formatRelativeTimeLabel(item.createdAt)}
          </time>
        </p>
      </div>
    );
  if (item.kind === "review") {
    const Icon =
      item.state === "approved"
        ? CheckIcon
        : item.state === "changes_requested"
          ? XIcon
          : MessageSquareIcon;
    return (
      <article
        role="listitem"
        data-text-surface="background"
        className="m-3 space-y-3 rounded-lg border border-border p-3"
      >
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <Icon
            aria-hidden="true"
            className={`size-4 ${item.state === "approved" ? "text-green-600 dark:text-green-400" : item.state === "changes_requested" ? "text-red-600 dark:text-red-400" : "text-muted-foreground"}`}
          />
          <PullRequestsActor actor={item.author} />
          <span>{REVIEW_LABELS[item.state]}</span>
          <time
            dateTime={item.submittedAt}
            title={item.submittedAt}
            className="text-xs text-muted-foreground"
          >
            {formatRelativeTimeLabel(item.submittedAt)}
          </time>
        </div>
        <PullRequestsMarkdown text={item.body} baseUrl={`${context.webUrl}/`} />
        <PullRequestsDismissReview
          review={item}
          permission={permissions.dismissReview}
          host={context.host}
          live={live}
        />
      </article>
    );
  }
  if (item.kind === "comment")
    return (
      <div
        role="listitem"
        data-text-surface="background"
        className="m-3 rounded-lg border border-border"
      >
        <CommentBody
          key={`${item.id}:${item.minimized}`}
          comment={item}
          minimized={item.minimized}
          baseUrl={`${context.webUrl}/`}
          permissions={permissions}
          projectRef={projectRef}
          number={number}
          context={context}
          url={url}
          live={live}
        />
      </div>
    );
  return (
    <article
      role="listitem"
      data-text-surface="background"
      className="m-3 overflow-hidden rounded-lg border border-border"
    >
      <header className="flex flex-wrap items-center gap-2 bg-muted/40 p-3 text-xs">
        <PullRequestsFileLink path={item.path}>
          {item.path}
          {item.line === null
            ? ""
            : `:${item.startLine !== null && item.startLine !== item.line ? `${item.startLine}–` : ""}${item.line}`}
        </PullRequestsFileLink>
        {item.isResolved ? (
          <span className="rounded border border-border px-1.5">Resolved</span>
        ) : null}
        {item.isOutdated ? (
          <span className="rounded border border-border px-1.5">Outdated</span>
        ) : null}
      </header>
      {item.diffHunk ? (
        <pre className="overflow-auto border-b border-border p-3 font-mono text-xs">
          {item.diffHunk}
        </pre>
      ) : null}
      {item.comments.map((comment) => (
        <div key={comment.id} className="border-b border-border last:border-b-0">
          <CommentBody
            key={`${comment.id}:${comment.minimized}`}
            minimized={comment.minimized}
            comment={comment}
            baseUrl={`${context.webUrl}/`}
            permissions={permissions}
            projectRef={projectRef}
            number={number}
            context={context}
            url={url}
            live={live}
          />
        </div>
      ))}
      <PullRequestsThreadActions
        thread={item}
        permissions={permissions}
        projectRef={projectRef}
        number={number}
        live={live}
      />
    </article>
  );
});
