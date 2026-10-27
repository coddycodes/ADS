import type {
  EnvironmentId,
  PullRequestsContext,
  PullRequestsListRow,
  ScopedProjectRef,
} from "@coddy/contracts";
import { Link } from "@tanstack/react-router";
import { CircleCheckIcon, CircleDotIcon, CircleXIcon, MessageSquareIcon } from "lucide-react";
import { memo } from "react";
import { formatChangeRequestNumber } from "@coddy/shared/sourceControl";
import { formatRelativeTimeLabel } from "../../../timestampFormat";
import { PullRequestsActor } from "../shared/PullRequestsActor";
import { PullRequestsLabelChip } from "../shared/PullRequestsLabelChip";
import { PullRequestsStateIcon } from "../shared/PullRequestsStateIcon";
import { useMergeRequestHoverPrefetch } from "../useMergeRequestHoverPrefetch";
import { rowStateIcon, rowTimeLabel } from "./pullRequestsList.logic";
const REVIEW_LABELS = {
  review_required: "Review required",
  approved: "Approved",
  changes_requested: "Changes requested",
};
const CHECKS = {
  success: { Icon: CircleCheckIcon, label: "passed", color: "text-green-600 dark:text-green-400" },
  failure: { Icon: CircleXIcon, label: "failed", color: "text-red-600 dark:text-red-400" },
  pending: { Icon: CircleDotIcon, label: "pending", color: "text-amber-600 dark:text-amber-400" },
  neutral: { Icon: CircleDotIcon, label: "neutral", color: "text-muted-foreground" },
};
export interface PullRequestsRowProps {
  row: PullRequestsListRow;
  projectRef: ScopedProjectRef;
  context: Extract<PullRequestsContext, { status: "available" }>;
  scope?: { environmentId: EnvironmentId; cwd: string };
}
export const PullRequestsRow = memo(function PullRequestsRow({
  row,
  projectRef,
  context,
  scope,
}: PullRequestsRowProps) {
  const hoverPrefetch = useMergeRequestHoverPrefetch({
    enabled: scope !== undefined && context.provider === "gitlab",
    environmentId: scope?.environmentId ?? projectRef.environmentId,
    cwd: scope?.cwd ?? "",
    number: row.number,
  });
  const combinedClosed = context.capabilities.closedTabIncludesMerged;
  const reference = formatChangeRequestNumber(context.provider, row.number);
  const check = row.checksSummary === null ? null : CHECKS[row.checksSummary];
  return (
    <div
      role="listitem"
      data-text-surface="background"
      className="flex h-14 min-w-0 items-center gap-3 border-b border-border/60 px-4"
      {...(scope !== undefined && context.provider === "gitlab" ? hoverPrefetch : {})}
    >
      <PullRequestsStateIcon state={rowStateIcon(row)} />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <Link
            className="min-w-0 truncate text-sm font-medium underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-ring"
            title={row.title}
            to="/project/$environmentId/$projectId/pull-requests/$number"
            params={{ ...projectRef, number: String(row.number) }}
            search={{ tab: "conversation" }}
          >
            {row.title}
          </Link>
          {row.isDraft ? (
            <span className="rounded border border-border px-1 text-xs text-muted-foreground">
              Draft
            </span>
          ) : null}
          <span className="flex min-w-0 gap-1 overflow-hidden">
            {row.labels.map((label) => (
              <PullRequestsLabelChip key={label.name} label={label} />
            ))}
          </span>
        </div>
        <div className="flex min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap text-xs text-muted-foreground">
          <span>
            {combinedClosed ? `${reference} opened` : `${reference} · created`} {rowTimeLabel(row)}{" "}
            by <PullRequestsActor actor={row.author} preferName={!combinedClosed} />
          </span>
          {row.reviewDecision ? <span>· {REVIEW_LABELS[row.reviewDecision]}</span> : null}
          {row.approvals ? (
            <span>
              · approvals {row.approvals.approved} of {row.approvals.required}
            </span>
          ) : null}
          {row.unresolvedThreads !== null ? (
            <span>· {row.unresolvedThreads} unresolved</span>
          ) : null}
          {!combinedClosed && Number.isFinite(Date.parse(row.updatedAt)) ? (
            <span>· updated {formatRelativeTimeLabel(row.updatedAt)}</span>
          ) : null}
        </div>
      </div>
      {check ? (
        <span
          role="img"
          aria-label={`${context.capabilities.vocabulary.checks} ${check.label}`}
          title={`${context.capabilities.vocabulary.checks} ${check.label}`}
          className={check.color}
        >
          <check.Icon aria-hidden="true" className="size-4" />
        </span>
      ) : null}
      {row.commentCount > 0 ? (
        <span
          aria-label={`${row.commentCount} comments`}
          className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground"
        >
          <MessageSquareIcon aria-hidden="true" className="size-3.5" />
          {row.commentCount}
        </span>
      ) : null}
    </div>
  );
});
