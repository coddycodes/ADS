import { squashAtomCommandFailure } from "@coddy/client-runtime/state/runtime";
import { PullRequestsOperationError } from "@coddy/contracts";
import * as Schema from "effect/Schema";
import { useContext, useEffect, type ReactNode } from "react";
import type { EnvironmentQueryView } from "../../../state/query";
import { Skeleton } from "../../ui/skeleton";
import { PullRequestsContextRefresh } from "../pullRequestsContextRefresh";
import { PermissionButton } from "../../ui/permission-button";
const isOperationError = Schema.is(PullRequestsOperationError);

export function PullRequestsQueryState({
  query,
  label,
  children,
}: {
  query: EnvironmentQueryView<unknown>;
  label: string;
  children: ReactNode;
}) {
  const failure =
    query.emission._tag === "Failure" ? squashAtomCommandFailure(query.emission) : null;
  const error = isOperationError(failure) ? failure : null;
  const refreshContext = useContext(PullRequestsContextRefresh);
  useEffect(() => {
    if (error?.code === "not_authenticated") refreshContext?.();
  }, [error, refreshContext]);
  const message = error?.message ?? query.error;
  const alert = (
    <div role="alert" className="space-y-3 p-4 text-sm">
      <p>{message}</p>
      {error?.hostDetail ? <p className="text-muted-foreground">{error.hostDetail}</p> : null}
      <PermissionButton
        permission={{ allowed: !query.isPending, reason: query.isPending ? "Loading…" : null }}
        onClick={query.refresh}
        variant="outline"
        size="sm"
      >
        Retry
      </PermissionButton>
    </div>
  );
  if (message !== null && query.data === null) return alert;
  if (message !== null && query.data !== null)
    return (
      <>
        {alert}
        {children}
      </>
    );
  if (query.data === null)
    return (
      <div role="status" className="space-y-3 p-4">
        <span className="text-sm text-muted-foreground">Loading {label}…</span>
        <Skeleton className="h-16 w-full" />
      </div>
    );
  return children;
}
