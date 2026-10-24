import type { RemoteUpdateSnapshot } from "@bibcode/contracts";
import {
  isRemoteUpdateRunActive,
  type RemoteUpdateRunState,
} from "@bibcode/client-runtime/state/remoteUpdateCoordinator";
import {
  REMOTE_UPDATE_CHECK_TIMEOUT_MS,
  type RemoteUpdateCheckState,
  isRemoteUpdateConnectionFailure,
  isRemoteUpdateUnchecked,
} from "@bibcode/client-runtime/state/remoteUpdates";
import * as Cause from "effect/Cause";
import type { AsyncResult } from "effect/reactivity";

import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  remoteUpdateActionLabel,
  remoteUpdateFailureMessage,
  remoteUpdateProgressLabel,
} from "./remoteUpdatePresentation";

export type ServerUpdateBadgeVariant =
  | "checking"
  | "not-checked"
  | "unreachable"
  | "check-failed"
  | "up-to-date"
  | "update-available"
  | "busy"
  | "manual"
  | "error";

/** What the client knows about one environment's update status right now. */
export interface ServerUpdateStatus {
  /** Latest `updater.status` snapshot; kept after a failed re-read. */
  readonly snapshot: RemoteUpdateSnapshot | null;
  /** A status read is in flight over a live connection. */
  readonly pending: boolean;
  /** Why the latest status read failed over a live connection; null otherwise. */
  readonly error: string | null;
  /** A check is running for this environment, whichever view started it. */
  readonly checking: boolean;
  /** Why the latest check failed on the current connection; null otherwise. */
  readonly checkError: string | null;
}

/** The reason a badge shows for a check that failed over a live connection. */
export function describeUpdateCheckFailure(cause: Cause.Cause<unknown>): string {
  const error = Cause.squash(cause);
  if (Cause.isTimeoutError(error)) {
    return `The update check timed out after ${REMOTE_UPDATE_CHECK_TIMEOUT_MS / 1000} seconds.`;
  }
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : "The update check failed.";
}

/**
 * One environment's update status as views show it. Offline, no read can run, and a
 * read that only lost its connection brings no news about the updater: in both cases
 * the last snapshot (if any) stays in place of "Checking…" or "Can't reach updater",
 * because the connection status already says what is wrong.
 */
export function serverUpdateStatusFromQuery(
  query: {
    readonly data: RemoteUpdateSnapshot | null;
    readonly emission: AsyncResult.AsyncResult<unknown, unknown>;
    readonly error: string | null;
    readonly isPending: boolean;
  },
  environment: { readonly connected: boolean; readonly check: RemoteUpdateCheckState },
): ServerUpdateStatus {
  const liveReadFailed =
    environment.connected &&
    query.emission._tag === "Failure" &&
    !isRemoteUpdateConnectionFailure(query.emission.cause);
  const { failure } = environment.check;
  return {
    snapshot: query.data,
    pending: environment.connected && query.isPending,
    error: liveReadFailed ? query.error : null,
    checking: environment.check.inFlight,
    checkError: failure === null ? null : describeUpdateCheckFailure(failure.cause),
  };
}

function snapshotVariant(snapshot: RemoteUpdateSnapshot): ServerUpdateBadgeVariant {
  switch (snapshot.state) {
    case "error":
      return "error";
    case "checking":
      return "checking";
    case "downloading":
    case "installing":
      return "busy";
    case "update-available":
      return "update-available";
    case "up-to-date":
      return "up-to-date";
    case "idle":
      return isRemoteUpdateUnchecked(snapshot) ? "not-checked" : "manual";
  }
}

/**
 * Null means there is nothing honest to show yet. A running check comes first; a failed
 * read wins over the last snapshot until a retry settles; the host's own activity wins
 * over an earlier failed check; a background re-read keeps showing the snapshot.
 */
export function serverUpdateBadgeVariant(
  status: ServerUpdateStatus,
): ServerUpdateBadgeVariant | null {
  const { snapshot, pending, error, checking, checkError } = status;
  if (checking) return "checking";
  if (error !== null) return pending ? "checking" : "unreachable";
  if (snapshot === null) {
    if (pending) return "checking";
    return checkError === null ? null : "check-failed";
  }
  const hostVariant = snapshotVariant(snapshot);
  const hostBusy = hostVariant === "checking" || hostVariant === "busy";
  return checkError !== null && !hostBusy ? "check-failed" : hostVariant;
}

const BADGE_LABELS: Record<ServerUpdateBadgeVariant, string> = {
  checking: "Checking…",
  "not-checked": "Not checked yet",
  unreachable: "Can't reach updater",
  "check-failed": "Check failed",
  "up-to-date": "Up to date",
  "update-available": "Update available",
  busy: "Updating…",
  manual: "Manual updates",
  error: "Update status error",
};

const BADGE_CLASSES: Record<ServerUpdateBadgeVariant, string> = {
  checking: "border-border text-muted-foreground animate-pulse",
  "not-checked": "border-border text-muted-foreground",
  unreachable: "border-destructive/40 text-destructive",
  "check-failed": "border-destructive/40 text-destructive",
  "up-to-date": "border-border text-muted-foreground",
  "update-available": "border-amber-500/40 text-amber-600 dark:text-amber-400",
  busy: "border-border text-muted-foreground animate-pulse",
  manual: "border-border text-muted-foreground",
  error: "border-destructive/40 text-destructive",
};

function badgeReason(variant: ServerUpdateBadgeVariant, status: ServerUpdateStatus) {
  switch (variant) {
    case "unreachable":
      return status.error;
    case "check-failed":
      return status.checkError;
    case "error":
      return status.snapshot?.error ?? "The host's updater reported an error.";
    default:
      return null;
  }
}

export interface ServerUpdateBadgeProps extends ServerUpdateStatus {
  /** The caller derives visibility with visibleRemoteUpdateRun. */
  readonly run?: RemoteUpdateRunState | null | undefined;
  /** Saved server name, used only to explain a failed run. */
  readonly name?: string | undefined;
  readonly onUpdate?: (() => void) | undefined;
  /** Re-reads the status; offered next to "Can't reach updater". */
  readonly onRetry?: (() => void) | undefined;
  /**
   * Re-runs the check; offered next to "Check failed" and "Update status error". Omit it
   * where the view already shows its own Check action.
   */
  readonly onCheckAgain?: (() => void) | undefined;
  /** Reopens confirmation with fresh counts after a failed run. */
  readonly onRetryRun?: (() => void) | undefined;
}

export function ServerUpdateBadge({
  run = null,
  name = "The server",
  onUpdate,
  onRetry,
  onCheckAgain,
  onRetryRun,
  ...status
}: ServerUpdateBadgeProps) {
  const variant =
    run === null
      ? serverUpdateBadgeVariant(status)
      : isRemoteUpdateRunActive(run)
        ? "busy"
        : run.phase === "failed"
          ? "error"
          : "up-to-date";
  if (variant === null) return null;
  const { snapshot } = status;
  if (
    run === null &&
    variant === "update-available" &&
    snapshot?.support.installMode === "interactive" &&
    snapshot.latestVersion !== null &&
    onUpdate !== undefined
  ) {
    return (
      <Button size="xs" variant="outline" onClick={onUpdate}>
        {remoteUpdateActionLabel(snapshot.latestVersion)}
      </Button>
    );
  }
  const label =
    run !== null
      ? (remoteUpdateProgressLabel(run) ?? "Update failed")
      : variant === "update-available" && snapshot?.latestVersion != null
        ? `Update to v${snapshot.latestVersion}`
        : BADGE_LABELS[variant];
  const reason =
    run === null
      ? badgeReason(variant, status)
      : run.phase === "failed"
        ? remoteUpdateFailureMessage(name, run.failure)
        : null;
  const badge = (
    <span
      data-variant={variant}
      className={cn(
        "inline-flex min-w-0 max-w-full items-center rounded border px-1.5 py-0.5 text-xs whitespace-normal break-words",
        BADGE_CLASSES[variant],
      )}
    >
      {label}
      {reason === null ? null : <span className="sr-only">: {reason}</span>}
    </span>
  );
  const explained =
    reason === null ? (
      badge
    ) : (
      <Tooltip>
        <TooltipTrigger render={badge} />
        <TooltipPopup side="top" className="max-w-80 whitespace-pre-wrap">
          {reason}
        </TooltipPopup>
      </Tooltip>
    );
  const action =
    run?.phase === "failed" && onRetryRun !== undefined ? (
      <Button size="xs" variant="outline" onClick={onRetryRun}>
        Retry
      </Button>
    ) : run !== null ? null : variant === "unreachable" && onRetry !== undefined ? (
      <Button size="xs" variant="outline" aria-label="Retry update status" onClick={onRetry}>
        Retry
      </Button>
    ) : (variant === "check-failed" || variant === "error") && onCheckAgain !== undefined ? (
      <Button size="xs" variant="outline" onClick={onCheckAgain}>
        Check again
      </Button>
    ) : null;
  if (action === null) return explained;
  return (
    <span className="inline-flex min-w-0 max-w-full flex-wrap items-center gap-1">
      {explained}
      {action}
    </span>
  );
}
