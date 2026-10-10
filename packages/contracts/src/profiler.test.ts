import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";
import { DEFAULT_SERVER_SETTINGS } from "./settings.ts";
import {
  PROFILER_CLIENT_BATCH_LIMITS,
  ProfilerClientBatch,
  ProfilerClientBatchInvalid,
  ProfilerClientBatchTooLarge,
  ProfilerStatus,
} from "./profiler.ts";
import { WsServerReportProfilerClientBatchRpc } from "./rpc.ts";

const decodeStatus = Schema.decodeUnknownSync(ProfilerStatus);
const decodeBatch = Schema.decodeUnknownSync(ProfilerClientBatch);
const decodeBatchError = Schema.decodeUnknownSync(WsServerReportProfilerClientBatchRpc.errorSchema);

describe("profiler contracts", () => {
  it("defaults the profiler off", () => {
    expect(DEFAULT_SERVER_SETTINGS.enableProfiler).toBe(false);
  });

  it("decodes a status with findings", () => {
    const status = decodeStatus({
      enabled: true,
      recordingSince: "2026-10-09T12:00:00.000Z",
      diskBytes: 1024,
      eventCount: 3,
      droppedEvents: 0,
      lastCrashAt: null,
      capabilities: { nativeDumps: "available", osCrashRecords: "available", storage: "ok" },
      findings: [
        {
          id: "f-1",
          fingerprint: "0123456789ab",
          kind: "rpc.slow",
          severity: "warn",
          firstSeen: "2026-10-09T12:00:00.000Z",
          lastSeen: "2026-10-09T12:01:00.000Z",
          count: 2,
          source: "server",
          title: "orchestration.dispatch took 12.3 s",
          summary: "server 11.9 s · network 0.3 s · render 0.1 s",
          state: "open",
        },
      ],
    });
    expect(status.findings[0]?.kind).toBe("rpc.slow");
  });

  it("tells a directory another process records to apart from failing writes", () => {
    const withStorage = (storage: string) => ({
      enabled: false,
      recordingSince: null,
      diskBytes: 0,
      eventCount: 0,
      droppedEvents: 0,
      lastCrashAt: null,
      capabilities: { nativeDumps: "unavailable", osCrashRecords: "available", storage },
      findings: [],
    });
    expect(decodeStatus(withStorage("locked")).capabilities.storage).toBe("locked");
    expect(decodeStatus(withStorage("failing")).capabilities.storage).toBe("failing");
    expect(() => decodeStatus(withStorage("busy"))).toThrow();
  });

  const batchWith = (records: number, histograms: number) => ({
    clientId: "c",
    capabilities: [],
    windowMs: 10_000,
    records: Array.from({ length: records }, () => ({
      correlationId: null,
      method: "server.getConfig",
      rttMs: 300,
      renderMs: 10,
      startedAtMs: 0,
    })),
    histograms: Array.from({ length: histograms }, () => ({
      method: "server.getConfig",
      buckets: [[3, 7]],
    })),
    stalls: { count: 0, totalMs: 0, maxMs: 0 },
    interactions: { count: 0, maxMs: 0 },
    jsHeapBytes: null,
  });

  it("declares the server's batch rejections as typed errors of the batch RPC", () => {
    expect(
      decodeBatchError({
        _tag: "ProfilerClientBatchTooLarge",
        message: "The batch is over 64 KiB.",
      }),
    ).toBeInstanceOf(ProfilerClientBatchTooLarge);
    expect(
      decodeBatchError({
        _tag: "ProfilerClientBatchInvalid",
        message: "windowMs is out of range.",
      }),
    ).toBeInstanceOf(ProfilerClientBatchInvalid);
  });

  it("accepts a client batch at the limits and rejects one over either", () => {
    const { records, histograms } = PROFILER_CLIENT_BATCH_LIMITS;
    expect(() => decodeBatch(batchWith(records, histograms))).not.toThrow();
    expect(() => decodeBatch(batchWith(records + 1, 0))).toThrow();
    expect(() => decodeBatch(batchWith(0, histograms + 1))).toThrow();
  });
});
