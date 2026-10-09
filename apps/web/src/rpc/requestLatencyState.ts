import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentRpcRequestObservation } from "@bibcode/client-runtime/rpc";
import { WS_METHODS } from "@bibcode/contracts";
import { Atom } from "effect/unstable/reactivity";

import { appAtomRegistry } from "./atomRegistry";

export const SLOW_RPC_ACK_THRESHOLD_MS = 15_000;
export const MAX_TRACKED_RPC_ACK_REQUESTS = 256;
let slowRpcAckThresholdMs = SLOW_RPC_ACK_THRESHOLD_MS;

export interface SlowRpcAckRequest {
  readonly requestId: string;
  readonly startedAt: string;
  readonly startedAtMs: number;
  readonly method: string;
  readonly environmentId: string;
  readonly thresholdMs: number;
}

interface PendingRpcAckRequest {
  readonly request: SlowRpcAckRequest;
  readonly timeoutId: ReturnType<typeof setTimeout>;
}

const pendingRpcAckRequests = new Map<string, PendingRpcAckRequest>();
const untrackedRpcAckMethods = new Set<string>([
  WS_METHODS.previewAutomationConnect,
  // Background profiling uploads; the user never waits on them.
  WS_METHODS.serverReportProfilerClientBatch,
]);

const slowRpcAckRequestsAtom = Atom.make<ReadonlyArray<SlowRpcAckRequest>>([]).pipe(
  Atom.keepAlive,
  Atom.withLabel("slow-rpc-ack-requests"),
);

export const hasSlowRpcAckRequestsAtom = Atom.make(
  (get) => get(slowRpcAckRequestsAtom).length > 0,
).pipe(Atom.keepAlive, Atom.withLabel("has-slow-rpc-ack-requests"));

function setSlowRpcAckRequests(requests: ReadonlyArray<SlowRpcAckRequest>) {
  appAtomRegistry.set(slowRpcAckRequestsAtom, [...requests]);
}

function getSlowRpcAckRequestsValue(): ReadonlyArray<SlowRpcAckRequest> {
  return appAtomRegistry.get(slowRpcAckRequestsAtom);
}

function shouldTrackRpcAck(method: string): boolean {
  return !method.includes("subscribe") && !untrackedRpcAckMethods.has(method);
}

export function getSlowRpcAckRequests(): ReadonlyArray<SlowRpcAckRequest> {
  return getSlowRpcAckRequestsValue();
}

export function trackRpcRequestSent(
  requestId: string,
  { method, environmentId }: EnvironmentRpcRequestObservation,
): void {
  if (!shouldTrackRpcAck(method)) {
    return;
  }

  clearTrackedRpcRequest(requestId);
  evictOldestPendingRpcRequestIfNeeded();

  const startedAtMs = Date.now();
  const request: SlowRpcAckRequest = {
    requestId,
    startedAt: new Date(startedAtMs).toISOString(),
    startedAtMs,
    method,
    environmentId,
    thresholdMs: slowRpcAckThresholdMs,
  };
  const timeoutId = setTimeout(() => {
    pendingRpcAckRequests.delete(requestId);
    appendSlowRpcAckRequest(request);
  }, slowRpcAckThresholdMs);

  pendingRpcAckRequests.set(requestId, {
    request,
    timeoutId,
  });
}

export function acknowledgeRpcRequest(requestId: string): void {
  clearTrackedRpcRequest(requestId);
  const slowRequests = getSlowRpcAckRequestsValue();
  if (!slowRequests.some((request) => request.requestId === requestId)) {
    return;
  }

  setSlowRpcAckRequests(slowRequests.filter((request) => request.requestId !== requestId));
}

export function clearAllTrackedRpcRequests(): void {
  for (const pending of pendingRpcAckRequests.values()) {
    clearTimeout(pending.timeoutId);
  }
  pendingRpcAckRequests.clear();
  setSlowRpcAckRequests([]);
}

function clearTrackedRpcRequest(requestId: string): void {
  const pending = pendingRpcAckRequests.get(requestId);
  if (!pending) {
    return;
  }

  clearTimeout(pending.timeoutId);
  pendingRpcAckRequests.delete(requestId);
}

function appendSlowRpcAckRequest(request: SlowRpcAckRequest): void {
  const requests = [...getSlowRpcAckRequestsValue(), request];
  if (requests.length <= MAX_TRACKED_RPC_ACK_REQUESTS) {
    setSlowRpcAckRequests(requests);
    return;
  }

  setSlowRpcAckRequests(requests.slice(-MAX_TRACKED_RPC_ACK_REQUESTS));
}

function evictOldestPendingRpcRequestIfNeeded(): void {
  while (pendingRpcAckRequests.size >= MAX_TRACKED_RPC_ACK_REQUESTS) {
    const oldestRequestId = pendingRpcAckRequests.keys().next().value;
    if (oldestRequestId === undefined) {
      return;
    }

    clearTrackedRpcRequest(oldestRequestId);
  }
}

export function resetRequestLatencyStateForTests(): void {
  slowRpcAckThresholdMs = SLOW_RPC_ACK_THRESHOLD_MS;
  clearAllTrackedRpcRequests();
}

export function setSlowRpcAckThresholdMsForTests(thresholdMs: number): void {
  slowRpcAckThresholdMs = thresholdMs;
}

export function useSlowRpcAckRequests(): ReadonlyArray<SlowRpcAckRequest> {
  return useAtomValue(slowRpcAckRequestsAtom);
}

export function useHasSlowRpcAckRequests(): boolean {
  return useAtomValue(hasSlowRpcAckRequestsAtom);
}
