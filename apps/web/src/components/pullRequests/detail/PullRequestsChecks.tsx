import { getChangeRequestTerminologyForKind } from "@coddy/shared/sourceControl";
import type { PullRequestsChecks as Checks, PullRequestsContext } from "@coddy/contracts";
import { CircleCheckIcon, CircleDotIcon, CircleMinusIcon, CircleXIcon } from "lucide-react";
import { memo } from "react";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { checksSummaryLabel } from "./pullRequestsDetail.logic";
const STATES = {
  pending: { Icon: CircleDotIcon, color: "text-amber-600 dark:text-amber-400" },
  success: { Icon: CircleCheckIcon, color: "text-green-600 dark:text-green-400" },
  failure: { Icon: CircleXIcon, color: "text-red-600 dark:text-red-400" },
  cancelled: { Icon: CircleXIcon, color: "text-muted-foreground" },
  skipped: { Icon: CircleMinusIcon, color: "text-muted-foreground" },
  neutral: { Icon: CircleMinusIcon, color: "text-muted-foreground" },
};
function duration(seconds: number) {
  const total = Math.max(0, Math.round(seconds));
  return total < 60 ? `${total}s` : `${Math.floor(total / 60)}m ${total % 60}s`;
}
const CheckGroup = memo(function CheckGroup({ group }: { group: Checks["groups"][number] }) {
  return (
    <details open className="rounded-lg border border-border">
      <summary className="cursor-pointer bg-muted/30 px-3 py-2 text-sm font-medium">
        {group.name} {group.checks.length}
      </summary>
      <ul>
        {group.checks.map((check) => {
          const state = STATES[check.state];
          return (
            <li
              key={`${check.name}:${check.url ?? check.startedAt ?? ""}`}
              className="flex items-center gap-3 border-t border-border p-3 text-sm"
            >
              <span role="img" aria-label={check.state} title={check.state} className={state.color}>
                <state.Icon aria-hidden="true" className="size-4" />
              </span>
              <span className="min-w-0 flex-1 break-words">
                {check.url ? (
                  <PullRequestsExternalLink href={check.url}>{check.name}</PullRequestsExternalLink>
                ) : (
                  check.name
                )}
              </span>
              {check.durationSeconds !== null ? (
                <span className="text-xs text-muted-foreground">
                  {duration(check.durationSeconds)}
                </span>
              ) : null}
            </li>
          );
        })}
      </ul>
    </details>
  );
});
export const PullRequestsChecks = memo(function PullRequestsChecks({
  checks,
  context,
}: {
  checks: Checks;
  context: Extract<PullRequestsContext, { status: "available" }>;
}) {
  const empty = checks.groups.every((group) => group.checks.length === 0);
  return (
    <section
      aria-label={context.capabilities.vocabulary.checks}
      className="min-h-0 flex-1 space-y-3 overflow-auto p-4"
      data-text-surface="background"
    >
      {checks.pipelineUrl ? (
        <p className="text-sm">
          <PullRequestsExternalLink href={checks.pipelineUrl}>
            Open pipeline
          </PullRequestsExternalLink>
        </p>
      ) : null}
      <p className="text-sm text-muted-foreground">
        {empty
          ? context.capabilities.vocabulary.checks === "Pipelines"
            ? `No pipeline for this ${getChangeRequestTerminologyForKind(context.provider).singular}`
            : "No checks reported"
          : checksSummaryLabel(checks)}
      </p>
      {checks.groups.map((group) => (
        <CheckGroup key={group.name} group={group} />
      ))}
    </section>
  );
});
