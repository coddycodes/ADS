import { environmentRpcKey } from "@coddy/client-runtime/state/runtime";
import type { EnvironmentId } from "@coddy/contracts";
import { FRESH_MS } from "./visibleRefresh.logic";

export const HOVER_PREFETCH_MS = 150;
export const MAX_HOVER_PREFETCHES = 2;

export function createPrefetchGate() {
  let inflight = 0;
  return {
    tryAcquire(): boolean {
      if (inflight >= MAX_HOVER_PREFETCHES) return false;
      inflight += 1;
      return true;
    },
    release(): void {
      inflight = Math.max(0, inflight - 1);
    },
  };
}

const gates = new Map<string, ReturnType<typeof createPrefetchGate>>();

export function prefetchGate(environmentId: string) {
  let gate = gates.get(environmentId);
  if (!gate) {
    gate = createPrefetchGate();
    gates.set(environmentId, gate);
  }
  return gate;
}

export type PrefetchedRead = "get" | "getTimeline";

const prefetchedAt = new Map<string, number>();

type PrefetchRequest = { readonly environmentId: EnvironmentId; readonly input: unknown };

function prefetchKey(read: PrefetchedRead, request: PrefetchRequest) {
  return `${read}:${environmentRpcKey(request)}`;
}

/** Records that a hover prefetch answered `read` for `request` at `at`. */
export function recordPrefetch(read: PrefetchedRead, request: PrefetchRequest, at: number): void {
  for (const [key, recordedAt] of prefetchedAt)
    if (at - recordedAt > FRESH_MS) prefetchedAt.delete(key);
  prefetchedAt.set(prefetchKey(read, request), at);
}

/** Whether a hover prefetch answered `read` for `request` within the last `FRESH_MS`. */
export function prefetchedWithin(
  read: PrefetchedRead,
  request: PrefetchRequest,
  now: number,
): boolean {
  const at = prefetchedAt.get(prefetchKey(read, request));
  return at !== undefined && now - at <= FRESH_MS;
}

export function resetPrefetchGates(): void {
  gates.clear();
  prefetchedAt.clear();
}
