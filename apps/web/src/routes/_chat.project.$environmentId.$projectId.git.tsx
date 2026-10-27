import { scopeProjectRef } from "@coddy/client-runtime/environment";
import type { EnvironmentId, ProjectId } from "@coddy/contracts";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo } from "react";

import { GitManagerPanel } from "../components/gitManager/GitManagerPanel";
import { useProject } from "../state/entities";
import { useEnvironmentQuery } from "../state/query";
import { environmentShell } from "../state/shell";
import { ChatRouteInset } from "./-ChatRouteInset";

const MISSING_PROJECT_REDIRECT_DELAY_MS = 1_000;

function GitManagerRouteView() {
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

  if (!projectExists) return null;
  return (
    <ChatRouteInset>
      <GitManagerPanel projectRef={projectRef} />
    </ChatRouteInset>
  );
}

export const Route = createFileRoute("/_chat/project/$environmentId/$projectId/git")({
  component: GitManagerRouteView,
});
