import {
  type EnvironmentConnectionPresentation,
  isConnectionUnavailable,
} from "@coddy/client-runtime/connection";
import type { EnvironmentId } from "@coddy/contracts";

import type { SidebarProjectAvailabilityView } from "../Sidebar.logic";
import { Button } from "../ui/button";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

export interface SidebarProjectAvailabilityProps {
  readonly view: SidebarProjectAvailabilityView;
  /** The environment the notice is about, when known: its label and live connection. */
  readonly environment?: {
    readonly label: string;
    readonly connection: EnvironmentConnectionPresentation;
  } | null;
  readonly showRetry: boolean;
  readonly showConnectionSettings: boolean;
  /** Links to Settings → Remote Servers, where the desktop manages remote connections. */
  readonly showOpenRemoteServers?: boolean;
  readonly onRetry: (environmentId: EnvironmentId) => void;
  readonly onOpenSettings: () => void;
  readonly onViewDiagnostics: () => void;
  readonly onAdoptStorage: (environmentId: EnvironmentId) => void;
  readonly onRecoverData?: ((environmentId: EnvironmentId) => void) | undefined;
}

/**
 * Shell kinds a down connection produces. A failed project-shell subscription
 * on a live connection produces them too, so they never decide alone that the
 * connection is down.
 */
function isConnectionKind(kind: SidebarProjectAvailabilityView["kind"]): boolean {
  return kind === "degraded" || kind === "unavailable" || kind === "configuration-error";
}

/** A stable element id for the notice's reason, from its environment id. */
function reasonElementId(environmentId: EnvironmentId): string {
  return `sidebar-availability-reason-${environmentId.replace(/[^A-Za-z0-9_-]/gu, "-")}`;
}

function availabilityCopy(view: SidebarProjectAvailabilityView): string | null {
  switch (view.kind) {
    case "available":
      return null;
    case "empty-confirmed":
      return "No projects yet";
    case "loading":
      return "Project data is still loading";
    case "degraded":
      return "Showing cached projects";
    case "storage-changed":
      return "Project data location changed";
    case "recovery-required":
      return "Project data needs recovery";
    case "unavailable":
      return "Projects are unavailable";
    case "configuration-error":
      return "Project data configuration needs attention";
  }
}

export function SidebarProjectAvailability({
  view,
  environment = null,
  showRetry,
  showConnectionSettings,
  showOpenRemoteServers = false,
  onRetry,
  onOpenSettings,
  onViewDiagnostics,
  onAdoptStorage,
  onRecoverData,
}: SidebarProjectAvailabilityProps) {
  // Only the environment's live connection says whether it is down; the shell
  // kind alone can also mean a failed project sync on a live connection.
  const connectionDown =
    environment !== null &&
    view.environmentId !== null &&
    isConnectionKind(view.kind) &&
    isConnectionUnavailable(environment.connection);
  const copy = connectionDown ? `${environment.label} is not connected.` : availabilityCopy(view);
  if (copy === null) {
    return null;
  }
  // A down connection's reason is the one the composer banner states in full
  // (the shell clears it while retrying), so here it stays one hover or focus
  // away. Any other error appears nowhere else and stays visible.
  const reason = connectionDown
    ? environment.connection.error
    : view.kind !== "empty-confirmed" && view.kind !== "loading"
      ? view.error
      : null;
  const reasonId =
    connectionDown && reason !== null && view.environmentId !== null
      ? reasonElementId(view.environmentId)
      : null;
  const canActOnEnvironment = view.environmentId !== null;
  const showRecoveryActions =
    view.kind === "degraded" ||
    view.kind === "storage-changed" ||
    view.kind === "recovery-required" ||
    view.kind === "unavailable" ||
    view.kind === "configuration-error";

  return (
    <div className="px-2 pt-4 text-center text-xs text-muted-foreground">
      {reasonId !== null ? (
        <div>
          <Tooltip>
            <TooltipTrigger
              render={
                <span
                  tabIndex={0}
                  aria-describedby={reasonId}
                  className="cursor-help underline decoration-dotted underline-offset-2"
                >
                  {copy}
                </span>
              }
            />
            <TooltipPopup className="max-w-80">{reason}</TooltipPopup>
          </Tooltip>
          {/* Always in the DOM, so focusing the line announces the reason. */}
          <span id={reasonId} className="sr-only">
            {reason}
          </span>
        </div>
      ) : (
        <div>{copy}</div>
      )}
      {reason !== null && reasonId === null ? (
        <div className="mt-1 break-words">{reason}</div>
      ) : null}
      {showRecoveryActions &&
      view.hasCachedProjects &&
      (view.kind !== "degraded" || connectionDown) ? (
        <div className="mt-1">Cached projects remain visible.</div>
      ) : null}
      {showRecoveryActions ? (
        <div className="mt-2 flex flex-wrap justify-center gap-1">
          {canActOnEnvironment && showRetry ? (
            <Button size="xs" variant="ghost" onClick={() => onRetry(view.environmentId!)}>
              Retry
            </Button>
          ) : null}
          {canActOnEnvironment && showOpenRemoteServers && connectionDown ? (
            <Button size="xs" variant="ghost" onClick={onOpenSettings}>
              Open Remote Servers
            </Button>
          ) : null}
          {(view.kind === "storage-changed" || view.kind === "recovery-required") &&
          canActOnEnvironment &&
          onRecoverData ? (
            <Button size="xs" variant="outline" onClick={() => onRecoverData(view.environmentId!)}>
              Recover data
            </Button>
          ) : null}
          {showConnectionSettings ? (
            <Button size="xs" variant="ghost" onClick={onOpenSettings}>
              Settings
            </Button>
          ) : null}
          <Button size="xs" variant="ghost" onClick={onViewDiagnostics}>
            Diagnostics
          </Button>
          {view.kind === "storage-changed" && canActOnEnvironment ? (
            <Button size="xs" variant="outline" onClick={() => onAdoptStorage(view.environmentId!)}>
              Use this data location
            </Button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
