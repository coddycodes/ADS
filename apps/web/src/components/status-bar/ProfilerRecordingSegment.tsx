import { useNavigate } from "@tanstack/react-router";
import { memo } from "react";

import { useEnvironments } from "../../state/environments";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

/**
 * Status bar marker shown while any connected environment records profiling data.
 * Clicking it opens Diagnostics, where the profiler is switched off. `memo` keeps the
 * status bar's own frequent re-renders out of it.
 */
export const ProfilerRecordingSegment = memo(function ProfilerRecordingSegment() {
  const navigate = useNavigate();
  const { environments } = useEnvironments();
  const recording = environments.filter(
    (environment) =>
      environment.connection.phase === "connected" &&
      environment.serverConfig?.settings.enableProfiler === true,
  );
  if (recording.length === 0) return null;

  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            aria-label="Profiler recording, open diagnostics settings"
            className="inline-flex h-5 shrink-0 items-center gap-1.5 rounded px-1 text-xs text-muted-foreground outline-none hover:bg-accent/70 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => void navigate({ to: "/settings/diagnostics" })}
          />
        }
      >
        <span aria-hidden className="size-1.5 rounded-full bg-destructive" />
        Profiler
      </TooltipTrigger>
      <TooltipPopup side="top" align="end">
        <div className="space-y-1 text-xs">
          <div className="font-medium">Recording on</div>
          <ul>
            {recording.map(({ environmentId, label }) => (
              <li key={environmentId}>{label.trim() ? label : environmentId}</li>
            ))}
          </ul>
        </div>
      </TooltipPopup>
    </Tooltip>
  );
});
