import type { EnvironmentId } from "@coddy/contracts";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { pullRequestsEnvironment } from "../../state/pullRequests";
import { HOVER_PREFETCH_MS, prefetchGate, recordPrefetch } from "./hoverPrefetch.logic";
import { usePullRequestsQuery } from "./shared/usePullRequestsQuery";

function answered(query: { data: unknown; error: string | null; isPending: boolean }): boolean {
  return query.data !== null && query.error === null && !query.isPending;
}

export function useMergeRequestHoverPrefetch({
  enabled,
  environmentId,
  cwd,
  number,
}: {
  enabled: boolean;
  environmentId: EnvironmentId;
  cwd: string;
  number: number;
}): { onPointerEnter(): void; onPointerLeave(): void } {
  const [armed, setArmed] = useState(false);
  const timerRef = useRef<number | null>(null);
  const gateRef = useRef<ReturnType<typeof prefetchGate> | null>(null);
  const request = useMemo(
    () => ({ environmentId, input: { cwd, number } }),
    [cwd, environmentId, number],
  );
  const detailAtom = useMemo(
    () => (armed ? pullRequestsEnvironment.get(request) : null),
    [armed, request],
  );
  const timelineAtom = useMemo(
    () => (armed ? pullRequestsEnvironment.getTimeline(request) : null),
    [armed, request],
  );
  const detailQuery = usePullRequestsQuery(detailAtom);
  const timelineQuery = usePullRequestsQuery(timelineAtom);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    timerRef.current = null;
  }, []);

  const releaseGate = useCallback(() => {
    gateRef.current?.release();
    gateRef.current = null;
  }, []);

  useEffect(() => {
    if (!armed) return;
    const detailSettled =
      !detailQuery.isPending && (detailQuery.data !== null || detailQuery.error !== null);
    const timelineSettled =
      !timelineQuery.isPending && (timelineQuery.data !== null || timelineQuery.error !== null);
    if (detailSettled && timelineSettled) releaseGate();
  }, [
    armed,
    detailQuery.data,
    detailQuery.error,
    detailQuery.isPending,
    releaseGate,
    timelineQuery.data,
    timelineQuery.error,
    timelineQuery.isPending,
  ]);

  // An open within FRESH_MS of an answer shows it instead of reading again.
  const detailAnswer = answered(detailQuery) ? detailQuery.data : null;
  const timelineAnswer = answered(timelineQuery) ? timelineQuery.data : null;
  useEffect(() => {
    if (detailAnswer !== null) recordPrefetch("get", request, Date.now());
  }, [detailAnswer, request]);
  useEffect(() => {
    if (timelineAnswer !== null) recordPrefetch("getTimeline", request, Date.now());
  }, [request, timelineAnswer]);

  useEffect(
    () => () => {
      clearTimer();
      if (armed) releaseGate();
    },
    [armed, clearTimer, releaseGate],
  );

  const onPointerEnter = useCallback(() => {
    if (!enabled || armed) return;
    clearTimer();
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      const gate = prefetchGate(environmentId);
      if (!gate.tryAcquire()) return;
      gateRef.current = gate;
      setArmed(true);
    }, HOVER_PREFETCH_MS);
  }, [armed, clearTimer, enabled, environmentId]);

  const onPointerLeave = useCallback(() => {
    if (armed) return;
    clearTimer();
  }, [armed, clearTimer]);

  return { onPointerEnter, onPointerLeave };
}
