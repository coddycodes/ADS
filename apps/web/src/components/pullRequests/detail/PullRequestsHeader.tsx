import type { PullRequestsDetail, PullRequestsContext, ScopedProjectRef } from "@coddy/contracts";
import { PullRequestsBaseBranchPicker } from "../edit/PullRequestsBaseBranchPicker";
import { PullRequestsCheckoutMenu } from "../PullRequestsCheckoutMenu";
import type { PullRequestsScope } from "../usePullRequestsAction";
import { PullRequestsTitleEditor } from "../edit/PullRequestsTitleEditor";
import { PullRequestsSecondaryActions } from "../edit/PullRequestsSecondaryActions";
import { CopyIcon, RefreshCwIcon } from "lucide-react";
import { memo, useState } from "react";
import { writeTextToClipboard } from "../../../hooks/useCopyToClipboard";
import { Button } from "../../ui/button";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { PermissionButton } from "../../ui/permission-button";
import { PullRequestsStateIcon } from "../shared/PullRequestsStateIcon";
import { headerSentence } from "./pullRequestsDetail.logic";
export interface PullRequestsHeaderProps {
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
  detail: PullRequestsDetail;
  /**
   * The base branch picker and secondary actions only ever act on a live
   * detail, never a snapshot. Omitting this falls back to `detail`, so
   * callers that have not adopted the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
  context: Extract<PullRequestsContext, { status: "available" }>;
  onRefresh: () => void;
  refreshing: boolean;
}
const STATES = {
  open: { label: "Open", color: "bg-green-600/10 text-green-700 dark:text-green-400" },
  draft: { label: "Draft", color: "bg-muted text-muted-foreground" },
  merged: { label: "Merged", color: "bg-purple-600/10 text-purple-700 dark:text-purple-400" },
  closed: { label: "Closed", color: "bg-red-600/10 text-red-700 dark:text-red-400" },
};
export const PullRequestsHeader = memo(function PullRequestsHeader({
  detail,
  live = detail,
  projectRef,
  scope,
  context,
  onRefresh,
  refreshing,
}: PullRequestsHeaderProps) {
  const [copyStatus, setCopyStatus] = useState<string | null>(null);
  const state = detail.state === "open" && detail.isDraft ? "draft" : detail.state;
  const gitlabSentence = context.provider === "gitlab";
  const branch = (name: string) => (
    <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{name}</code>
  );
  return (
    <header className="shrink-0 space-y-3 p-4">
      <PullRequestsTitleEditor
        detail={detail}
        live={live}
        projectRef={projectRef}
        provider={context.provider}
      />
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span
          className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 font-medium ${STATES[state].color}`}
        >
          <PullRequestsStateIcon state={state} />
          <span>{STATES[state].label}</span>
        </span>
        <div
          className="min-w-0 break-words text-muted-foreground"
          aria-label={headerSentence(detail, context.provider)}
        >
          <span className="font-medium text-foreground">{detail.author.login}</span>{" "}
          {gitlabSentence ? (
            <>
              requested to merge {branch(detail.headBranch)} into{" "}
              <PullRequestsBaseBranchPicker
                detail={detail}
                live={live}
                scope={scope}
                projectRef={projectRef}
              />
            </>
          ) : (
            <>
              wants to merge {detail.commitCount} commit{detail.commitCount === 1 ? "" : "s"} into{" "}
              <PullRequestsBaseBranchPicker
                detail={detail}
                live={live}
                scope={scope}
                projectRef={projectRef}
              />{" "}
              from {branch(detail.headBranch)}
            </>
          )}
        </div>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Copy head branch"
          title="Copy head branch"
          onClick={() => {
            void writeTextToClipboard(detail.headBranch, "branch name").then(
              () => setCopyStatus("Branch copied"),
              () => setCopyStatus("Copy failed. Select and copy the branch name."),
            );
          }}
        >
          <CopyIcon aria-hidden="true" />
        </Button>
        {detail.isCrossRepository ? (
          <span className="text-xs text-muted-foreground">from a fork</span>
        ) : null}
        {detail.locked ? (
          <span className="text-xs text-muted-foreground">
            Locked{detail.lockReason ? `: ${detail.lockReason}` : ""}
          </span>
        ) : null}
      </div>
      {copyStatus ? (
        <p role="status" className="text-xs text-muted-foreground">
          {copyStatus}
        </p>
      ) : null}
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <PullRequestsExternalLink href={detail.url}>Open in browser</PullRequestsExternalLink>
        <PullRequestsCheckoutMenu
          scope={scope}
          projectRef={projectRef}
          number={detail.number}
          headBranch={detail.headBranch}
          permission={detail.permissions.checkout}
          live={live !== null}
        />
        <PermissionButton
          permission={{ allowed: !refreshing, reason: refreshing ? "Refreshing…" : null }}
          onClick={onRefresh}
          variant="outline"
          size="sm"
        >
          <RefreshCwIcon aria-hidden="true" />
          Refresh
        </PermissionButton>
        <PullRequestsSecondaryActions detail={detail} live={live} context={context} />
      </div>
    </header>
  );
});
