import type { EnvironmentThreadShell } from "@coddy/client-runtime/state/models";
import type { ScopedProjectRef } from "@coddy/contracts";
import { BotIcon } from "lucide-react";
import { memo, useMemo } from "react";

import { useThreadShellsForProjectRefs } from "../../../state/entities";

export interface GitManagerAgentActivityProps {
  readonly projectRef: ScopedProjectRef;
  readonly cwd: string;
  readonly mainCheckoutCwd: string;
}

function isActiveInCheckout(
  shell: EnvironmentThreadShell,
  cwd: string,
  mainCheckoutCwd: string,
): boolean {
  const status = shell.session?.status;
  if (status !== "starting" && status !== "running") return false;
  return shell.worktreePath === null ? cwd === mainCheckoutCwd : shell.worktreePath === cwd;
}

export const GitManagerAgentActivity = memo(function GitManagerAgentActivity({
  projectRef,
  cwd,
  mainCheckoutCwd,
}: GitManagerAgentActivityProps) {
  const { environmentId, projectId } = projectRef;
  const projectRefs = useMemo(
    () => [{ environmentId, projectId }] as ReadonlyArray<ScopedProjectRef>,
    [environmentId, projectId],
  );
  const shells = useThreadShellsForProjectRefs(projectRefs);
  const activeCount = useMemo(
    () => shells.filter((shell) => isActiveInCheckout(shell, cwd, mainCheckoutCwd)).length,
    [cwd, mainCheckoutCwd, shells],
  );

  if (activeCount === 0) return null;
  const label = `${activeCount} agent session${activeCount === 1 ? "" : "s"} active`;
  return (
    <span
      aria-label={label}
      className="inline-flex h-6 items-center gap-1.5 rounded-full border border-border bg-muted/50 px-2 text-xs text-muted-foreground"
      data-testid="git-manager-agent-activity"
      title={label}
    >
      <BotIcon aria-hidden="true" className="size-3.5" />
      {label}
    </span>
  );
});
