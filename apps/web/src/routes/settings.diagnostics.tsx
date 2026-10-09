import { EnvironmentId } from "@bibcode/contracts";
import { createFileRoute } from "@tanstack/react-router";
import { useCallback } from "react";

import { DiagnosticsSettingsPanel } from "../components/settings/DiagnosticsSettings";

export interface DiagnosticsSearch {
  /** The environment whose profiler the page shows, such as the one the status bar named. */
  readonly environment?: string;
}

export function validateDiagnosticsSearch(search: Record<string, unknown>): DiagnosticsSearch {
  return typeof search.environment === "string" && search.environment.length > 0
    ? { environment: search.environment }
    : {};
}

export const Route = createFileRoute("/settings/diagnostics")({
  validateSearch: validateDiagnosticsSearch,
  component: DiagnosticsRouteView,
});

function DiagnosticsRouteView() {
  const { environment } = Route.useSearch();
  const navigate = Route.useNavigate();
  const selectEnvironment = useCallback(
    (environmentId: EnvironmentId | null) => {
      void navigate({
        search: environmentId === null ? {} : { environment: environmentId },
        replace: true,
      });
    },
    [navigate],
  );
  return (
    <DiagnosticsSettingsPanel
      profilerEnvironmentId={environment === undefined ? null : EnvironmentId.make(environment)}
      onProfilerEnvironmentChange={selectEnvironment}
    />
  );
}
