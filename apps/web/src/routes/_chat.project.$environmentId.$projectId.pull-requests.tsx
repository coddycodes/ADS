import { scopeProjectRef } from "@coddy/client-runtime/environment";
import type { EnvironmentId, ProjectId } from "@coddy/contracts";
import { createFileRoute, Outlet, useMatch, useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo } from "react";

import { PullRequestsPanel } from "../components/pullRequests/PullRequestsPanel";
import { useProject } from "../state/entities";
import { useEnvironmentQuery } from "../state/query";
import { environmentShell } from "../state/shell";
import { ChatRouteInset } from "./-ChatRouteInset";

const MISSING_PROJECT_REDIRECT_DELAY_MS = 1_000;

function PullRequestsListRouteView() {
  const detailMatch = useMatch({
    from: "/_chat/project/$environmentId/$projectId/pull-requests/$number",
    shouldThrow: false,
  });
  const navigate = useNavigate();
  const params = Route.useParams();
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

  if (detailMatch) return <Outlet />;
  if (!projectExists) return null;
  return (
    <ChatRouteInset>
      <PullRequestsPanel projectRef={projectRef} />
    </ChatRouteInset>
  );
}

export const Route = createFileRoute("/_chat/project/$environmentId/$projectId/pull-requests")({
  component: PullRequestsListRouteView,
});
