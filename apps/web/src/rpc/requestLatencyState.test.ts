import { WS_METHODS } from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { appAtomRegistry } from "./atomRegistry";
import {
  acknowledgeRpcRequest,
  getSlowRpcAckRequests,
  hasSlowRpcAckRequestsAtom,
  resetRequestLatencyStateForTests,
  trackRpcRequestSent,
  SLOW_RPC_ACK_THRESHOLD_MS,
  MAX_TRACKED_RPC_ACK_REQUESTS,
} from "./requestLatencyState";

describe("requestLatencyState", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetRequestLatencyStateForTests();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("marks unary requests as slow when the ack threshold is exceeded", () => {
    trackRpcRequestSent("1", { method: "server.getConfig", environmentId: "primary" });
    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS - 1);
    expect(getSlowRpcAckRequests()).toEqual([]);

    vi.advanceTimersByTime(1);
    expect(getSlowRpcAckRequests()).toMatchObject([
      {
        requestId: "1",
        method: "server.getConfig",
        environmentId: "primary",
        thresholdMs: SLOW_RPC_ACK_THRESHOLD_MS,
      },
    ]);
  });

  it("clears the slow request once the server acknowledges it", () => {
    trackRpcRequestSent("1", { method: "git.status", environmentId: "primary" });
    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS);
    expect(getSlowRpcAckRequests()).toHaveLength(1);

    acknowledgeRpcRequest("1");
    expect(getSlowRpcAckRequests()).toEqual([]);
  });

  it("ignores long-lived subscribe requests", () => {
    trackRpcRequestSent("1", { method: "subscribeServerConfig", environmentId: "primary" });
    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS * 2);

    expect(getSlowRpcAckRequests()).toEqual([]);
  });

  it("ignores the long-lived preview automation connection", () => {
    trackRpcRequestSent("1", {
      method: WS_METHODS.previewAutomationConnect,
      environmentId: "remote-1",
    });
    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS * 2);

    expect(getSlowRpcAckRequests()).toEqual([]);
  });

  it("ignores the profiler's own batch uploads", () => {
    trackRpcRequestSent("1", {
      method: WS_METHODS.serverReportProfilerClientBatch,
      environmentId: "primary",
    });
    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS * 2);

    expect(getSlowRpcAckRequests()).toEqual([]);
  });

  it("evicts the oldest pending requests once the tracker reaches capacity", () => {
    for (let index = 0; index < MAX_TRACKED_RPC_ACK_REQUESTS + 1; index += 1) {
      trackRpcRequestSent(String(index), { method: "server.getConfig", environmentId: "primary" });
    }

    vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS);

    const slowRequests = getSlowRpcAckRequests();
    expect(slowRequests).toHaveLength(MAX_TRACKED_RPC_ACK_REQUESTS);
    expect(slowRequests[0]?.requestId).toBe("1");
    expect(slowRequests.at(-1)?.requestId).toBe(String(MAX_TRACKED_RPC_ACK_REQUESTS));
  });

  it("notifies boolean subscribers only when slowness starts or ends", () => {
    const values: boolean[] = [];
    const unsubscribe = appAtomRegistry.subscribe(
      hasSlowRpcAckRequestsAtom,
      (value) => values.push(value),
      { immediate: true },
    );
    try {
      expect(values).toEqual([false]);

      trackRpcRequestSent("1", { method: "server.getConfig", environmentId: "primary" });
      vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS);
      expect(values).toEqual([false, true]);

      trackRpcRequestSent("2", { method: "git.status", environmentId: "remote-1" });
      vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS);
      expect(getSlowRpcAckRequests()).toHaveLength(2);
      expect(values).toEqual([false, true]);

      acknowledgeRpcRequest("1");
      expect(getSlowRpcAckRequests()).toHaveLength(1);
      expect(values).toEqual([false, true]);

      acknowledgeRpcRequest("2");
      expect(values).toEqual([false, true, false]);
    } finally {
      unsubscribe();
    }
  });
});
