import { scopeProjectRef } from "@coddy/client-runtime/environment";
import type { EnvironmentId, ProjectId } from "@coddy/contracts";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo } from "react";

import { PullRequestsPanel } from "../components/pullRequests/PullRequestsPanel";
import { useProject } from "../state/entities";
import { useEnvironmentQuery } from "../state/query";
import { environmentShell } from "../state/shell";
import { ChatRouteInset } from "./-ChatRouteInset";

const MISSING_PROJECT_REDIRECT_DELAY_MS = 1_000;

function PullRequestsDetailRouteView() {
  const navigate = useNavigate();
  const params = Route.useParams();
  const search = Route.useSearch();
  const { environmentId, projectId } = params;
  const projectRef = useMemo(
    () => scopeProjectRef(environmentId as EnvironmentId, projectId as ProjectId),
    [environmentId, projectId],
  );
  const project = useProject(projectRef);
  const shell = useEnvironmentQuery(environmentShell.stateAtom(projectRef.environmentId));
  const shellStatus = shell.data?.status ?? null;
  const projectExists = project !== null;

  useEffect(() => {
    if (projectExists || shellStatus !== "live") return;
    const timeoutId = window.setTimeout(() => {
      void navigate({ to: "/", replace: true });
    }, MISSING_PROJECT_REDIRECT_DELAY_MS);
    return () => window.clearTimeout(timeoutId);
  }, [navigate, projectExists, shellStatus]);

  if (!projectExists) return null;
  return (
    <ChatRouteInset>
      <PullRequestsPanel projectRef={projectRef} number={Number(params.number)} tab={search.tab} />
    </ChatRouteInset>
  );
}

export const Route = createFileRoute(
  "/_chat/project/$environmentId/$projectId/pull-requests/$number",
)({
  validateSearch: (
    search: Record<string, unknown>,
  ): { tab: "conversation" | "commits" | "checks" | "files" } => ({
    tab:
      search.tab === "commits" || search.tab === "checks" || search.tab === "files"
        ? search.tab
        : "conversation",
  }),
  beforeLoad: ({ params }) => {
    if (
      !/^\d+$/.test(params.number) ||
      !Number.isSafeInteger(Number(params.number)) ||
      Number(params.number) < 1
    ) {
      throw redirect({
        to: "/project/$environmentId/$projectId/pull-requests",
        params: { environmentId: params.environmentId, projectId: params.projectId },
        replace: true,
      });
    }
  },
  component: PullRequestsDetailRouteView,
});
