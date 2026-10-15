import type { EnvironmentId } from "@bibcode/contracts";
import { useCallback, useEffect, useEffectEvent, useMemo } from "react";
import { pullRequestsEnvironment } from "../../state/pullRequests";
import { useEnvironmentQuery } from "../../state/query";

/**
 * Reads the repository context of one checkout: a stored context paints at once and is
 * read live right after. Shared by every host of the Pull Requests views.
 */
export function usePullRequestsContextQuery({
  environmentId,
  cwd,
  enabled,
}: {
  environmentId: EnvironmentId;
  cwd: string | null;
  enabled: boolean;
}) {
  const contextAtom = useMemo(
    () =>
      enabled && cwd !== null
        ? pullRequestsEnvironment.getContext({ environmentId, input: { cwd } })
        : null,
    [enabled, cwd, environmentId],
  );
  const query = useEnvironmentQuery(contextAtom);
  const refreshContext = query.refresh;
  const revalidateContext = query.revalidate;
  // A stored context paints at once and is read live right after; a failed live read
  // keeps the stored context on screen instead of replacing the panel with an error.
  const storedContext = query.data?.status === "available" && query.data.stale === true;
  // Every stored answer is followed by one live read, including one that arrives
  // again after a failed live read or for another checkout.
  const storedAnswer = storedContext ? query.data : null;
  const contextReading = query.isPending;
  const readContextLive = useEffectEvent(() => {
    // A panel remounted by a route change joins the read already running.
    if (!contextReading) revalidateContext();
  });
  useEffect(() => {
    if (storedAnswer !== null) readContextLive();
  }, [storedAnswer]);
  // Rescan and auth recovery bypass the server's bounded context caches; opening the
  // panel and switching checkout reuse them.
  const requestContextRescan = useCallback(() => {
    if (cwd !== null)
      pullRequestsEnvironment.requestContextRescan({ environmentId, input: { cwd } });
  }, [cwd, environmentId]);
  const rescan = useCallback(() => {
    requestContextRescan();
    refreshContext();
  }, [refreshContext, requestContextRescan]);
  // Auth recovery follows a read failure automatically, so it keeps a cut-off latched.
  const recover = useCallback(() => {
    requestContextRescan();
    revalidateContext();
  }, [requestContextRescan, revalidateContext]);
  return { contextAtom, query, storedContext, rescan, recover };
}
