import { PullRequestsPicker } from "../edit/PullRequestsPicker";
import { inverseOf, scheduleUndo } from "../edit/undoToast.logic";
import { toastManager } from "../../ui/toast";
import type { PullRequestsAction, PullRequestsScope } from "../usePullRequestsAction";
import { usePullRequestsActions } from "../usePullRequestsAction";
import type { PullRequestsContext, PullRequestsDetail, ScopedProjectRef } from "@coddy/contracts";
import {
  RefreshCcwIcon,
  CheckIcon,
  CircleIcon,
  MessageSquareIcon,
  MinusIcon,
  XIcon,
} from "lucide-react";
import { memo, useMemo, type ReactNode } from "react";
import { PullRequestsActor } from "../shared/PullRequestsActor";
import { PullRequestsLabelChip } from "../shared/PullRequestsLabelChip";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
const REVIEW_STATES = {
  unreviewed: { Icon: CircleIcon, label: "Unreviewed", color: "text-muted-foreground" },
  commented: { Icon: MessageSquareIcon, label: "Commented", color: "text-muted-foreground" },
  approved: { Icon: CheckIcon, label: "Approved", color: "text-green-600 dark:text-green-400" },
  changes_requested: {
    Icon: XIcon,
    label: "Changes requested",
    color: "text-red-600 dark:text-red-400",
  },
  dismissed: { Icon: MinusIcon, label: "Dismissed", color: "text-muted-foreground line-through" },
  review_started: { Icon: CircleIcon, label: "Review started", color: "text-muted-foreground" },
};
function Section({
  title,
  editor,
  children,
}: {
  title: string;
  editor?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="space-y-2 border-b border-border pb-3 last:border-b-0">
      <header className="flex items-center justify-between gap-2">
        <h2 className="text-sm font-semibold">{title}</h2>
        {editor}
      </header>
      <div className="space-y-2 text-xs">{children}</div>
    </section>
  );
}
const NONE = <p className="text-muted-foreground">None</p>;
export const PullRequestsSideColumn = memo(function PullRequestsSideColumn({
  detail,
  live = detail,
  context,
  scope,
  detailsRefreshing = false,
}: {
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
  detail: PullRequestsDetail;
  /**
   * Every picker and the re-request review button only ever act on a live
   * detail, never a snapshot. Omitting this falls back to `detail`, so
   * callers that have not adopted the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
  context: Extract<PullRequestsContext, { status: "available" }>;
  detailsRefreshing?: boolean;
}) {
  const { run, pending } = usePullRequestsActions();
  // Stable during command-state renders; every new detail receipt reconciles picker state,
  // including an Undo that returns the host to exactly the previous selection.
  const selected = useMemo(
    () => ({
      reviewers: detail.reviewers.map((reviewer) => reviewer.actor.login),
      assignees: detail.assignees.map((actor) => actor.login),
      labels: detail.labels.map((label) => label.name),
      milestone: detail.milestone ? [detail.milestone.id] : [],
    }),
    [detail],
  );
  async function apply(
    action: PullRequestsAction,
    title: string,
    previousMilestone: string | null = null,
  ) {
    await run(action);
    const inverse = inverseOf(action, { milestoneId: previousMilestone });
    if (inverse) scheduleUndo(run, inverse, toastManager, title);
  }
  function changes(add: string[], remove: string[], noun: string) {
    return [
      add.length ? `Added ${add.length} ${noun}${add.length === 1 ? "" : "s"}` : "",
      remove.length ? `Removed ${remove.length} ${noun}${remove.length === 1 ? "" : "s"}` : "",
    ]
      .filter(Boolean)
      .join("; ");
  }
  const available = (permission: PullRequestsDetail["permissions"]["editLabels"]) =>
    constrainPermission(
      permission,
      live === null
        ? "Loading…"
        : pending
          ? "Wait for the current action to finish"
          : detailsRefreshing
            ? "Refreshing request details…"
            : null,
    );
  return (
    <aside aria-label="Request details" className="space-y-4 p-3" data-text-surface="background">
      <Section
        title={context.capabilities.vocabulary.reviewer}
        editor={
          <PullRequestsPicker
            scope={scope}
            label={context.capabilities.vocabulary.reviewer}
            kind="users"
            multiple
            selected={selected.reviewers}
            exclude={[detail.author.login]}
            permission={available(detail.permissions.editReviewers)}
            onChange={(add, remove) =>
              apply({ action: "setReviewers", add, remove }, changes(add, remove, "reviewer"))
            }
          />
        }
      >
        {detail.reviewers.length === 0
          ? NONE
          : detail.reviewers.map(({ actor, state, canRerequest }) => {
              const presentation = REVIEW_STATES[state];
              return (
                <div key={actor.login} className="flex items-center gap-2">
                  <span
                    role="img"
                    aria-label={presentation.label}
                    title={presentation.label}
                    className={presentation.color}
                  >
                    <presentation.Icon aria-hidden="true" className="size-4" />
                  </span>
                  <span className={state === "dismissed" ? "line-through" : undefined}>
                    <PullRequestsActor actor={actor} />
                  </span>
                  {canRerequest ? (
                    <PermissionButton
                      mutation
                      permission={available(detail.permissions.rerequestReview)}
                      aria-label={`Re-request review from ${actor.login}`}
                      size="icon-sm"
                      variant="ghost"
                      onClick={() => {
                        void run({ action: "rerequestReview", login: actor.login }).catch(
                          () => undefined,
                        );
                      }}
                    >
                      <RefreshCcwIcon aria-hidden="true" />
                    </PermissionButton>
                  ) : null}
                </div>
              );
            })}
      </Section>
      <Section
        title="Assignees"
        editor={
          <PullRequestsPicker
            scope={scope}
            label="Assignees"
            kind="users"
            multiple
            selected={selected.assignees}
            permission={available(detail.permissions.editAssignees)}
            onChange={(add, remove) =>
              apply({ action: "setAssignees", add, remove }, changes(add, remove, "assignee"))
            }
          />
        }
      >
        {detail.assignees.length === 0
          ? NONE
          : detail.assignees.map((actor) => (
              <div key={actor.login}>
                <PullRequestsActor actor={actor} />
              </div>
            ))}
      </Section>
      <Section
        title="Labels"
        editor={
          <PullRequestsPicker
            scope={scope}
            label="Labels"
            kind="labels"
            multiple
            selected={selected.labels}
            permission={available(detail.permissions.editLabels)}
            onChange={(add, remove) =>
              apply({ action: "setLabels", add, remove }, changes(add, remove, "label"))
            }
          />
        }
      >
        {detail.labels.length === 0 ? (
          NONE
        ) : (
          <div className="flex flex-wrap gap-1">
            {detail.labels.map((label) => (
              <PullRequestsLabelChip key={label.name} label={label} />
            ))}
          </div>
        )}
      </Section>
      <Section
        title="Milestone"
        editor={
          <PullRequestsPicker
            scope={scope}
            label="Milestone"
            kind="milestones"
            multiple={false}
            selected={selected.milestone}
            permission={available(detail.permissions.editMilestone)}
            onChange={(add, remove) =>
              apply(
                { action: "setMilestone", milestoneId: add[0] ?? null },
                add.length ? "Milestone set" : "Milestone cleared",
                remove[0] ?? null,
              )
            }
          />
        }
      >
        {detail.milestone ? <p>{detail.milestone.title}</p> : NONE}
      </Section>
      <Section title="Linked issues">
        <p className="text-muted-foreground">From the description</p>
        {detail.linkedIssues.length === 0
          ? NONE
          : detail.linkedIssues.map((issue) => (
              <p key={issue.reference}>
                <PullRequestsExternalLink href={issue.url}>
                  {issue.reference}
                  {issue.title ? ` ${issue.title}` : ""}
                </PullRequestsExternalLink>
              </p>
            ))}
      </Section>
      {context.capabilities.mergeMethodsSource === "project_setting" ? (
        <Section title="Approval rules">
          {detail.approvalRules.length === 0
            ? NONE
            : detail.approvalRules.map((rule) => (
                <div key={rule.name}>
                  <p className="font-medium">
                    {rule.name}: {rule.approved} of {rule.required}
                  </p>
                  {rule.approvers.map((actor) => (
                    <div key={actor.login}>
                      <PullRequestsActor actor={actor} />
                    </div>
                  ))}
                </div>
              ))}
        </Section>
      ) : null}
    </aside>
  );
});
