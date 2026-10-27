import type {
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerProcessResourceHistorySummary,
} from "@coddy/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { describe, expect, it } from "vite-plus/test";

import {
  HISTORY_PROCESS_COLUMNS,
  LIVE_PROCESS_COLUMNS,
  RESOURCE_HISTORY_WINDOWS,
  presentLiveProcesses,
  presentResourceHistory,
  toggleHistoryProcessSort,
  toggleLiveProcessSort,
} from "./resourceDiagnosticsPresentation";

const T0 = DateTime.makeUnsafe("2026-07-19T15:00:00.000Z");
const T1 = DateTime.makeUnsafe("2026-07-19T15:01:00.000Z");

function processEntry(
  patch: Partial<ServerProcessDiagnosticsEntry> &
    Pick<ServerProcessDiagnosticsEntry, "pid" | "processKey" | "scope" | "kind" | "label">,
): ServerProcessDiagnosticsEntry {
  return {
    ppid: 100,
    pgid: Option.none(),
    status: "Run",
    cpuPercent: 1,
    rssBytes: 10 * 1024 ** 2,
    elapsed: "00:00:01",
    command: `command-for-${patch.pid}`,
    depth: 1,
    childPids: [],
    confidence: "exact",
    ...patch,
  };
}

function liveFixture(
  patch: Partial<ServerProcessDiagnosticsResult> = {},
): ServerProcessDiagnosticsResult {
  const processes = [
    processEntry({
      pid: 100,
      ppid: 1,
      processKey: "100:1",
      scope: "core",
      kind: "server",
      label: "Coddy Studio Server",
      depth: 0,
      childPids: [200, 300],
      cpuPercent: 2,
      rssBytes: 200 * 1024 ** 2,
      command: "coddy-studio server",
    }),
    processEntry({
      pid: 200,
      processKey: "200:1",
      scope: "external",
      kind: "provider",
      label: "Codex",
      childPids: [201],
      cpuPercent: 9,
      rssBytes: 300 * 1024 ** 2,
      command: "codex app-server",
    }),
    processEntry({
      pid: 201,
      ppid: 200,
      processKey: "201:1",
      scope: "external",
      kind: "provider",
      label: "Codex worker",
      depth: 2,
      cpuPercent: 3,
      rssBytes: 80 * 1024 ** 2,
      command: "node codex-worker.js",
      confidence: "inherited",
    }),
    processEntry({
      pid: 300,
      processKey: "300:1",
      scope: "core",
      kind: "ui",
      label: "Coddy Studio UI",
      cpuPercent: 1,
      rssBytes: 120 * 1024 ** 2,
      command: "Coddy Studio WebContent",
    }),
    processEntry({
      pid: 400,
      ppid: 1,
      processKey: "400:1",
      scope: "external",
      kind: "helper",
      label: "Reparented helper",
      depth: 0,
      childPids: [401],
      cpuPercent: 4,
      rssBytes: 60 * 1024 ** 2,
      command: "helper --serve",
    }),
    processEntry({
      pid: 401,
      ppid: 400,
      processKey: "401:1",
      scope: "external",
      kind: "helper",
      label: "Reparented child",
      depth: 1,
      cpuPercent: 4,
      rssBytes: 40 * 1024 ** 2,
      command: "helper-child",
      confidence: "inherited",
    }),
  ];

  return {
    serverPid: 100,
    readAt: T0,
    totals: {
      combined: { processCount: 6, rssBytes: 800 * 1024 ** 2, cpuPercent: 23 },
      core: { processCount: 2, rssBytes: 320 * 1024 ** 2, cpuPercent: 3 },
      external: { processCount: 4, rssBytes: 480 * 1024 ** 2, cpuPercent: 20 },
    },
    uiCoverage: { status: "available", message: Option.none() },
    processes,
    error: Option.none(),
    ...patch,
  };
}

function historySummary(
  patch: Partial<ServerProcessResourceHistorySummary> = {},
): ServerProcessResourceHistorySummary {
  return {
    processKey: "100:1",
    pid: 100,
    ppid: 1,
    command: "coddy-studio server",
    depth: 0,
    scope: "core",
    kind: "server",
    label: "Coddy Studio Server",
    confidence: "exact",
    firstSeenAt: T0,
    lastSeenAt: T1,
    currentCpuPercent: 2,
    avgCpuPercent: 1.5,
    maxCpuPercent: 8,
    cpuSecondsApprox: 65,
    currentRssBytes: 100 * 1024 ** 2,
    maxRssBytes: 200 * 1024 ** 2,
    sampleCount: 10,
    ...patch,
  };
}

function historyFixture(
  patch: Partial<ServerProcessResourceHistoryResult> = {},
): ServerProcessResourceHistoryResult {
  return {
    readAt: T1,
    windowMs: 15 * 60_000,
    bucketMs: 60_000,
    sampleIntervalMs: 2_000,
    retainedSampleCount: 120,
    cpuSecondsApprox: { combined: 75, core: 65, external: 10 },
    uiCoverage: { status: "available", message: Option.none() },
    buckets: [
      {
        startedAt: T0,
        endedAt: T1,
        cpuPercent: {
          average: { combined: 10, core: 6, external: 4 },
          peak: { combined: 40, core: 25, external: 15 },
        },
        rssBytes: {
          average: {
            combined: 800 * 1024 ** 2,
            core: 500 * 1024 ** 2,
            external: 300 * 1024 ** 2,
          },
          peak: {
            combined: 1024 * 1024 ** 2,
            core: 600 * 1024 ** 2,
            external: 424 * 1024 ** 2,
          },
        },
        maxProcessCount: { combined: 6, core: 2, external: 4 },
      },
    ],
    processes: [
      historySummary(),
      historySummary({
        processKey: "200:1",
        pid: 200,
        ppid: 100,
        command: "codex app-server",
        depth: 1,
        scope: "external",
        kind: "provider",
        label: "Codex",
        confidence: "exact",
        currentCpuPercent: 9,
        avgCpuPercent: 7,
        maxCpuPercent: 30,
        cpuSecondsApprox: 10,
        currentRssBytes: 250 * 1024 ** 2,
        maxRssBytes: 400 * 1024 ** 2,
      }),
    ],
    error: Option.none(),
    ...patch,
  };
}

describe("presentLiveProcesses", () => {
  it("uses server totals for Combined, Coddy Studio Core, and External Tooling summaries", () => {
    const diagnostics = liveFixture({
      processes: liveFixture().processes.slice(0, 2),
    });

    const presentation = presentLiveProcesses({ diagnostics, queryError: null });

    expect(presentation.summary).toEqual({
      combined: {
        title: "Combined",
        memoryLabel: "800.0 MB",
        cpuLabel: "23.0%",
        processCountLabel: "6",
      },
      core: {
        title: "Coddy Studio Core",
        memoryLabel: "320.0 MB",
        cpuLabel: "3.0%",
        processCountLabel: "2",
      },
      external: {
        title: "External Tooling",
        memoryLabel: "480.0 MB",
        cpuLabel: "20.0%",
        processCountLabel: "4",
      },
    });
  });

  it("defaults to descending memory order without deriving totals from visible rows", () => {
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture(),
      queryError: null,
    });

    expect(presentation.sort).toEqual({ key: "memory", direction: "desc" });
    expect(presentation.rows.map((row) => row.pid)).toEqual([200, 100, 300, 201, 400, 401]);
  });

  it.each([
    [{ key: "memory", direction: "asc" }, [401, 400, 201, 300, 100, 200]],
    [{ key: "cpu", direction: "desc" }, [200, 400, 401, 201, 100, 300]],
    [{ key: "name", direction: "asc" }, [100, 300, 200, 201, 401, 400]],
    [{ key: "scope", direction: "asc" }, [100, 300, 200, 201, 400, 401]],
  ] as const)("sorts by %o", (sort, expectedPids) => {
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture(),
      queryError: null,
      sort,
    });

    expect(presentation.rows.map((row) => row.pid)).toEqual(expectedPids);
  });

  it("toggles the selected sort and gives new text columns ascending defaults", () => {
    expect(toggleLiveProcessSort({ key: "memory", direction: "desc" }, "memory")).toEqual({
      key: "memory",
      direction: "asc",
    });
    expect(toggleLiveProcessSort({ key: "memory", direction: "desc" }, "cpu")).toEqual({
      key: "cpu",
      direction: "desc",
    });
    expect(toggleLiveProcessSort({ key: "cpu", direction: "desc" }, "name")).toEqual({
      key: "name",
      direction: "asc",
    });
    expect(toggleLiveProcessSort({ key: "name", direction: "asc" }, "scope")).toEqual({
      key: "scope",
      direction: "asc",
    });
  });

  it("breaks equal primary values deterministically by process key", () => {
    const tied = liveFixture({
      processes: [
        processEntry({
          pid: 9,
          processKey: "9:1",
          scope: "external",
          kind: "helper",
          label: "same",
        }),
        processEntry({
          pid: 2,
          processKey: "2:1",
          scope: "external",
          kind: "helper",
          label: "same",
        }),
      ],
    });

    expect(
      presentLiveProcesses({ diagnostics: tied, queryError: null }).rows.map(
        (row) => row.processKey,
      ),
    ).toEqual(["2:1", "9:1"]);
  });

  it("totally orders non-finite live CPU values with process-key ties", () => {
    const process = (processKey: string, cpuPercent: number) =>
      processEntry({
        pid: 1,
        processKey,
        scope: "external",
        kind: "helper",
        label: "same",
        cpuPercent,
      });
    const processes = [
      process("nan-b", Number.NaN),
      process("positive-infinity-b", Number.POSITIVE_INFINITY),
      process("finite-b", 5),
      process("negative-infinity-b", Number.NEGATIVE_INFINITY),
      process("nan-a", Number.NaN),
      process("positive-infinity-a", Number.POSITIVE_INFINITY),
      process("finite-a", 5),
      process("negative-infinity-a", Number.NEGATIVE_INFINITY),
    ];

    const ascending = presentLiveProcesses({
      diagnostics: liveFixture({ processes }),
      queryError: null,
      sort: { key: "cpu", direction: "asc" },
    });
    const descending = presentLiveProcesses({
      diagnostics: liveFixture({ processes }),
      queryError: null,
      sort: { key: "cpu", direction: "desc" },
    });

    expect(ascending.rows.map((row) => row.processKey)).toEqual([
      "negative-infinity-a",
      "negative-infinity-b",
      "finite-a",
      "finite-b",
      "positive-infinity-a",
      "positive-infinity-b",
      "nan-a",
      "nan-b",
    ]);
    expect(descending.rows.map((row) => row.processKey)).toEqual([
      "nan-a",
      "nan-b",
      "positive-infinity-a",
      "positive-infinity-b",
      "finite-a",
      "finite-b",
      "negative-infinity-a",
      "negative-infinity-b",
    ]);
  });

  it("presents non-finite live CPU values as unavailable", () => {
    const diagnostics = liveFixture({
      totals: {
        combined: { processCount: 3, rssBytes: 30, cpuPercent: Number.NaN },
        core: { processCount: 1, rssBytes: 10, cpuPercent: Number.POSITIVE_INFINITY },
        external: {
          processCount: 2,
          rssBytes: 20,
          cpuPercent: Number.NEGATIVE_INFINITY,
        },
      },
      processes: [
        processEntry({
          pid: 1,
          processKey: "nan",
          scope: "core",
          kind: "server",
          label: "NaN",
          cpuPercent: Number.NaN,
        }),
        processEntry({
          pid: 2,
          processKey: "positive-infinity",
          scope: "external",
          kind: "provider",
          label: "Positive infinity",
          cpuPercent: Number.POSITIVE_INFINITY,
        }),
        processEntry({
          pid: 3,
          processKey: "negative-infinity",
          scope: "external",
          kind: "helper",
          label: "Negative infinity",
          cpuPercent: Number.NEGATIVE_INFINITY,
        }),
      ],
    });

    const presentation = presentLiveProcesses({ diagnostics, queryError: null });

    expect(presentation.summary?.combined.cpuLabel).toBe("Unavailable");
    expect(presentation.summary?.core.cpuLabel).toBe("Unavailable");
    expect(presentation.summary?.external.cpuLabel).toBe("Unavailable");
    expect(presentation.rows.map((row) => row.cpuLabel)).toEqual([
      "Unavailable",
      "Unavailable",
      "Unavailable",
    ]);
  });

  it("presents every attributed live column", () => {
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture(),
      queryError: null,
    });
    const codex = presentation.rows.find((row) => row.pid === 200);

    expect(LIVE_PROCESS_COLUMNS).toEqual([
      "Scope",
      "Kind",
      "Label",
      "Command",
      "CPU",
      "Memory",
      "PID",
    ]);
    expect(codex).toMatchObject({
      scopeLabel: "External",
      kindLabel: "Provider",
      label: "Codex",
      command: "codex app-server",
      cpuLabel: "9.0%",
      memoryLabel: "300.0 MB",
      pid: 200,
    });
  });

  it("only enables signals for External rows with current ancestry to the server", () => {
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture(),
      queryError: null,
    });
    const signalEligibility = Object.fromEntries(
      presentation.rows.map((row) => [row.processKey, row.canSignal]),
    );

    expect(signalEligibility).toEqual({
      "100:1": false,
      "200:1": true,
      "201:1": true,
      "300:1": false,
      "400:1": false,
      "401:1": false,
    });
  });

  it("does not enable signals through ancestry cycles or reparented roots", () => {
    const base = liveFixture();
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture({
        processes: [
          ...base.processes,
          processEntry({
            pid: 500,
            ppid: 501,
            processKey: "500:1",
            scope: "external",
            kind: "helper",
            label: "Cycle A",
          }),
          processEntry({
            pid: 501,
            ppid: 500,
            processKey: "501:1",
            scope: "external",
            kind: "helper",
            label: "Cycle B",
          }),
        ],
      }),
      queryError: null,
    });
    const signalEligibility = Object.fromEntries(
      presentation.rows.map((row) => [row.processKey, row.canSignal]),
    );

    expect(signalEligibility["400:1"]).toBe(false);
    expect(signalEligibility["401:1"]).toBe(false);
    expect(signalEligibility["500:1"]).toBe(false);
    expect(signalEligibility["501:1"]).toBe(false);
  });

  it("makes every duplicate target PID identity unsignalable", () => {
    const base = liveFixture();
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture({
        processes: [
          ...base.processes,
          processEntry({
            pid: 200,
            ppid: 100,
            processKey: "200:2",
            scope: "external",
            kind: "provider",
            label: "Reused Codex PID",
          }),
          processEntry({
            pid: 600,
            ppid: 100,
            processKey: "600:1",
            scope: "external",
            kind: "helper",
            label: "Independent helper",
          }),
        ],
      }),
      queryError: null,
    });
    const signalEligibility = Object.fromEntries(
      presentation.rows.map((row) => [row.processKey, row.canSignal]),
    );

    expect(signalEligibility["200:1"]).toBe(false);
    expect(signalEligibility["200:2"]).toBe(false);
    expect(signalEligibility["600:1"]).toBe(true);
  });

  it("makes descendants of an ambiguous parent PID unsignalable", () => {
    const base = liveFixture();
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture({
        processes: [
          ...base.processes,
          processEntry({
            pid: 700,
            ppid: 100,
            processKey: "700:1",
            scope: "external",
            kind: "terminal",
            label: "Terminal identity A",
          }),
          processEntry({
            pid: 700,
            ppid: 100,
            processKey: "700:2",
            scope: "external",
            kind: "terminal",
            label: "Terminal identity B",
          }),
          processEntry({
            pid: 701,
            ppid: 700,
            processKey: "701:1",
            scope: "external",
            kind: "terminal",
            label: "Terminal child",
          }),
          processEntry({
            pid: 702,
            ppid: 100,
            processKey: "702:1",
            scope: "external",
            kind: "helper",
            label: "Independent helper",
          }),
        ],
      }),
      queryError: null,
    });
    const signalEligibility = Object.fromEntries(
      presentation.rows.map((row) => [row.processKey, row.canSignal]),
    );

    expect(signalEligibility["700:1"]).toBe(false);
    expect(signalEligibility["700:2"]).toBe(false);
    expect(signalEligibility["701:1"]).toBe(false);
    expect(signalEligibility["702:1"]).toBe(true);
  });

  it("retains the server sample timestamp while marking last-good data stale", () => {
    const presentation = presentLiveProcesses({
      diagnostics: liveFixture(),
      queryError: "The diagnostics connection was lost.",
    });

    expect(presentation.checkedAt).toBe(T0);
    expect(presentation.availability).toBe("stale");
    expect(presentation.summary?.combined.memoryLabel).toBe("800.0 MB");
    expect(presentation.banners[0]).toMatchObject({
      tone: "warning",
      statusLabel: "Showing stale resource data",
    });
  });

  it("uses unavailable placeholders without turning a failed sample into zeroes", () => {
    const failed = liveFixture({
      totals: {
        combined: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
        core: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
        external: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
      },
      processes: [],
      error: Option.some({ message: "No process sample is available." }),
    });

    const presentation = presentLiveProcesses({
      diagnostics: failed,
      queryError: null,
    });

    expect(presentation.checkedAt).toBe(T0);
    expect(presentation.availability).toBe("unavailable");
    expect(presentation.summary).toBeNull();
    expect(presentation.rows).toEqual([]);
    expect(presentation.banners[0]).toMatchObject({
      tone: "danger",
      statusLabel: "Resource data unavailable",
      message: "No process sample is available.",
    });
  });
});

describe("presentResourceHistory", () => {
  it("treats a first sampling failure as unavailable even when the server supplies zero buckets", () => {
    const zeroBucket = historyFixture().buckets[0]!;
    const presentation = presentResourceHistory({
      history: historyFixture({
        retainedSampleCount: 0,
        cpuSecondsApprox: { combined: 0, core: 0, external: 0 },
        buckets: [
          {
            ...zeroBucket,
            cpuPercent: {
              average: { combined: 0, core: 0, external: 0 },
              peak: { combined: 0, core: 0, external: 0 },
            },
            rssBytes: {
              average: { combined: 0, core: 0, external: 0 },
              peak: { combined: 0, core: 0, external: 0 },
            },
            maxProcessCount: { combined: 0, core: 0, external: 0 },
          },
        ],
        processes: [],
        error: Option.some({
          failureTag: "ProcessDiagnosticsQueryFailedError",
          message: "process diagnostics sampling failed: initial failure",
        }),
      }),
      queryError: null,
    });

    expect(presentation.availability).toBe("unavailable");
    expect(presentation.summary).toBeNull();
    expect(presentation.chart.bars).toEqual([]);
    expect(presentation.rows).toEqual([]);
    expect(presentation.banners[0]).toMatchObject({
      tone: "danger",
      statusLabel: "Resource data unavailable",
      message: "process diagnostics sampling failed: initial failure",
    });
  });

  it("keeps retained successful history visible and stale after a later sampling failure", () => {
    const presentation = presentResourceHistory({
      history: historyFixture({
        error: Option.some({
          failureTag: "ProcessDiagnosticsQueryFailedError",
          message: "process diagnostics sampling failed: refresh failed",
        }),
      }),
      queryError: null,
    });

    expect(presentation.availability).toBe("stale");
    expect(presentation.summary?.combined.valueLabel).toBe("1.25m");
    expect(presentation.chart.bars).toHaveLength(1);
    expect(presentation.rows).toHaveLength(2);
    expect(presentation.banners[0]).toMatchObject({
      tone: "warning",
      statusLabel: "Showing stale resource data",
      message:
        "Showing the last successful server sample. process diagnostics sampling failed: refresh failed",
    });
  });

  it("defaults to Memory and stacks additive Core and External bucket averages", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
    });

    expect(presentation.metric).toBe("memory");
    expect(presentation.chart.bars[0]?.average).toEqual({
      combined: 800 * 1024 ** 2,
      core: 500 * 1024 ** 2,
      external: 300 * 1024 ** 2,
    });
    expect(presentation.chart.bars[0]?.average.combined).toBe(
      (presentation.chart.bars[0]?.average.core ?? 0) +
        (presentation.chart.bars[0]?.average.external ?? 0),
    );
  });

  it("selects split CPU percentages for the CPU metric", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
      metric: "cpu",
    });

    expect(presentation.metric).toBe("cpu");
    expect(presentation.chart.bars[0]?.average).toEqual({
      combined: 10,
      core: 6,
      external: 4,
    });
    expect(presentation.chart.bars[0]?.peak).toEqual({
      combined: 40,
      core: 25,
      external: 15,
    });
  });

  it("presents Combined average and same-sample Combined/Core/External peak in tooltips", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
    });
    const bar = presentation.chart.bars[0];

    expect(bar?.peak).toEqual({
      combined: 1024 * 1024 ** 2,
      core: 600 * 1024 ** 2,
      external: 424 * 1024 ** 2,
    });
    expect(bar?.tooltip).toContain("Combined average 800.0 MB");
    expect(bar?.tooltip).toContain("Same-sample peak 1.0 GB");
    expect(bar?.tooltip).toContain("Core 600.0 MB");
    expect(bar?.tooltip).toContain("External 424.0 MB");
  });

  it("splits history summary CPU time into Combined, Core, and External cards", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
    });

    expect(presentation.summary).toEqual({
      combined: { title: "Combined", valueLabel: "1.25m" },
      core: { title: "Coddy Studio Core", valueLabel: "1.08m" },
      external: { title: "External Tooling", valueLabel: "10.0s" },
    });
  });

  it("retains complete attributed process metrics and columns", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
    });
    const codex = presentation.rows.find((row) => row.pid === 200);

    expect(HISTORY_PROCESS_COLUMNS).toEqual([
      "Scope",
      "Kind",
      "Label",
      "CPU Time",
      "Current CPU",
      "Average CPU",
      "Peak CPU",
      "Max Memory",
      "Command",
      "PID",
    ]);
    expect(codex).toMatchObject({
      scopeLabel: "External",
      kindLabel: "Provider",
      label: "Codex",
      cpuTimeLabel: "10.0s",
      currentCpuLabel: "9.0%",
      averageCpuLabel: "7.0%",
      peakCpuLabel: "30.0%",
      maxMemoryLabel: "400.0 MB",
      command: "codex app-server",
      pid: 200,
    });
    expect(codex).not.toHaveProperty("currentMemoryLabel");
  });

  it("defaults history processes to descending maximum-memory order", () => {
    const presentation = presentResourceHistory({
      history: historyFixture(),
      queryError: null,
    });

    expect(presentation.processSort).toEqual({ key: "maxMemory", direction: "desc" });
    expect(presentation.rows.map((row) => row.processKey)).toEqual(["200:1", "100:1"]);
  });

  it.each([
    [{ key: "label", direction: "asc" }, ["1:1", "2:1", "3:1"]],
    [{ key: "scope", direction: "asc" }, ["1:1", "2:1", "3:1"]],
    [{ key: "kind", direction: "asc" }, ["3:1", "2:1", "1:1"]],
    [{ key: "cpuTime", direction: "desc" }, ["2:1", "1:1", "3:1"]],
    [{ key: "currentCpu", direction: "desc" }, ["3:1", "1:1", "2:1"]],
    [{ key: "averageCpu", direction: "desc" }, ["2:1", "1:1", "3:1"]],
    [{ key: "peakCpu", direction: "desc" }, ["3:1", "1:1", "2:1"]],
    [{ key: "maxMemory", direction: "desc" }, ["1:1", "3:1", "2:1"]],
  ] as const)("sorts full history rows by %o with process-key ties", (processSort, expected) => {
    const processes = [
      historySummary({
        processKey: "1:1",
        pid: 1,
        scope: "core",
        kind: "ui",
        label: "Alpha",
        cpuSecondsApprox: 20,
        currentCpuPercent: 5,
        avgCpuPercent: 6,
        maxCpuPercent: 8,
        maxRssBytes: 300 * 1024 ** 2,
      }),
      historySummary({
        processKey: "2:1",
        pid: 2,
        scope: "external",
        kind: "provider",
        label: "Beta",
        cpuSecondsApprox: 30,
        currentCpuPercent: 1,
        avgCpuPercent: 9,
        maxCpuPercent: 7,
        maxRssBytes: 100 * 1024 ** 2,
      }),
      historySummary({
        processKey: "3:1",
        pid: 3,
        scope: "external",
        kind: "helper",
        label: "Gamma",
        cpuSecondsApprox: 10,
        currentCpuPercent: 10,
        avgCpuPercent: 2,
        maxCpuPercent: 12,
        maxRssBytes: 200 * 1024 ** 2,
      }),
    ];
    const presentation = presentResourceHistory({
      history: historyFixture({ processes }),
      queryError: null,
      processSort,
    });

    expect(presentation.rows.map((row) => row.processKey)).toEqual(expected);
    expect(presentation.rows).toHaveLength(processes.length);
  });

  it("toggles history text sorts ascending and metric sorts descending", () => {
    expect(toggleHistoryProcessSort({ key: "maxMemory", direction: "desc" }, "maxMemory")).toEqual({
      key: "maxMemory",
      direction: "asc",
    });
    expect(toggleHistoryProcessSort({ key: "maxMemory", direction: "desc" }, "label")).toEqual({
      key: "label",
      direction: "asc",
    });
    expect(toggleHistoryProcessSort({ key: "label", direction: "asc" }, "cpuTime")).toEqual({
      key: "cpuTime",
      direction: "desc",
    });
  });

  it("breaks equal history sort values by process key", () => {
    const presentation = presentResourceHistory({
      history: historyFixture({
        processes: [
          historySummary({ processKey: "9:1", pid: 9, maxRssBytes: 100 }),
          historySummary({ processKey: "2:1", pid: 2, maxRssBytes: 100 }),
        ],
      }),
      queryError: null,
    });

    expect(presentation.rows.map((row) => row.processKey)).toEqual(["2:1", "9:1"]);
  });

  it.each([
    ["cpuTime", "cpuSecondsApprox"],
    ["currentCpu", "currentCpuPercent"],
    ["averageCpu", "avgCpuPercent"],
    ["peakCpu", "maxCpuPercent"],
  ] as const)(
    "totally orders non-finite %s values in both directions with process-key ties",
    (key, field) => {
      const process = (processKey: string, value: number) =>
        historySummary({
          processKey,
          pid: 1,
          [field]: value,
        });
      const processes = [
        process("nan-b", Number.NaN),
        process("positive-infinity-b", Number.POSITIVE_INFINITY),
        process("finite-b", 5),
        process("negative-infinity-b", Number.NEGATIVE_INFINITY),
        process("nan-a", Number.NaN),
        process("positive-infinity-a", Number.POSITIVE_INFINITY),
        process("finite-a", 5),
        process("negative-infinity-a", Number.NEGATIVE_INFINITY),
      ];

      const ascending = presentResourceHistory({
        history: historyFixture({ processes }),
        queryError: null,
        processSort: { key, direction: "asc" },
      });
      const descending = presentResourceHistory({
        history: historyFixture({ processes }),
        queryError: null,
        processSort: { key, direction: "desc" },
      });

      expect(ascending.rows.map((row) => row.processKey)).toEqual([
        "negative-infinity-a",
        "negative-infinity-b",
        "finite-a",
        "finite-b",
        "positive-infinity-a",
        "positive-infinity-b",
        "nan-a",
        "nan-b",
      ]);
      expect(descending.rows.map((row) => row.processKey)).toEqual([
        "nan-a",
        "nan-b",
        "positive-infinity-a",
        "positive-infinity-b",
        "finite-a",
        "finite-b",
        "negative-infinity-a",
        "negative-infinity-b",
      ]);
    },
  );

  it("presents non-finite history metrics as bounded unavailable labels", () => {
    const bucket = historyFixture().buckets[0]!;
    const presentation = presentResourceHistory({
      history: historyFixture({
        cpuSecondsApprox: {
          combined: Number.NaN,
          core: Number.POSITIVE_INFINITY,
          external: Number.NEGATIVE_INFINITY,
        },
        processes: [
          historySummary({
            cpuSecondsApprox: Number.NaN,
            currentCpuPercent: Number.POSITIVE_INFINITY,
            avgCpuPercent: Number.NEGATIVE_INFINITY,
            maxCpuPercent: Number.NaN,
            maxRssBytes: Number.POSITIVE_INFINITY,
          }),
        ],
        buckets: [
          {
            ...bucket,
            cpuPercent: {
              average: {
                combined: Number.NaN,
                core: Number.POSITIVE_INFINITY,
                external: Number.NEGATIVE_INFINITY,
              },
              peak: {
                combined: Number.POSITIVE_INFINITY,
                core: Number.NEGATIVE_INFINITY,
                external: Number.NaN,
              },
            },
          },
        ],
      }),
      queryError: null,
      metric: "cpu",
    });

    expect(presentation.summary).toEqual({
      combined: { title: "Combined", valueLabel: "Unavailable" },
      core: { title: "Coddy Studio Core", valueLabel: "Unavailable" },
      external: { title: "External Tooling", valueLabel: "Unavailable" },
    });
    expect(presentation.rows[0]).toMatchObject({
      cpuTimeLabel: "Unavailable",
      currentCpuLabel: "Unavailable",
      averageCpuLabel: "Unavailable",
      peakCpuLabel: "Unavailable",
      maxMemoryLabel: "Unavailable",
    });
    expect(presentation.chart.bars[0]).toMatchObject({
      average: { combined: 0, core: 0, external: 0 },
      peak: { combined: 0, core: 0, external: 0 },
      averageLabels: {
        combined: "Unavailable",
        core: "Unavailable",
        external: "Unavailable",
      },
      peakLabels: {
        combined: "Unavailable",
        core: "Unavailable",
        external: "Unavailable",
      },
    });
    expect(presentation.chart.maximumAverage).toBe(1);
  });

  it("preserves negative finite bucket diagnostics and tooltip values", () => {
    const bucket = historyFixture().buckets[0]!;
    const presentation = presentResourceHistory({
      history: historyFixture({
        buckets: [
          {
            ...bucket,
            cpuPercent: {
              average: { combined: 10, core: -6, external: 16 },
              peak: { combined: 40, core: -5, external: 45 },
            },
          },
        ],
      }),
      queryError: null,
      metric: "cpu",
    });

    expect(presentation.chart.bars[0]).toMatchObject({
      average: { combined: 10, core: -6, external: 16 },
      peak: { combined: 40, core: -5, external: 45 },
      averageLabels: { combined: "10.0%", core: "-6.0%", external: "16.0%" },
      peakLabels: { combined: "40.0%", core: "-5.0%", external: "45.0%" },
      tooltip: "Combined average 10.0%. Same-sample peak 40.0%: Core -5.0%, External 45.0%.",
    });
  });

  it("preserves the existing resource window inputs", () => {
    expect(RESOURCE_HISTORY_WINDOWS).toEqual([
      { label: "5m", windowMs: 5 * 60_000, bucketMs: 30_000 },
      { label: "15m", windowMs: 15 * 60_000, bucketMs: 60_000 },
      { label: "30m", windowMs: 30 * 60_000, bucketMs: 2 * 60_000 },
      { label: "1h", windowMs: 60 * 60_000, bucketMs: 5 * 60_000 },
    ]);
  });

  it.each([
    ["partial", "Some local UI processes could not be sampled."],
    ["unavailable", "Local UI process accounting is unavailable."],
  ] as const)("explains %s coverage without implying complete Core totals", (status, message) => {
    const presentation = presentResourceHistory({
      history: historyFixture({
        uiCoverage: { status, message: Option.some(message) },
      }),
      queryError: null,
    });

    expect(presentation.banners).toContainEqual(
      expect.objectContaining({
        tone: "warning",
        statusLabel: status === "partial" ? "Partial UI coverage" : "UI coverage unavailable",
        message: expect.stringMatching(/Core and Combined.*omit unobserved UI/i),
      }),
    );
  });

  it("treats not-applicable UI coverage as neutral for a headless server", () => {
    const presentation = presentResourceHistory({
      history: historyFixture({
        uiCoverage: { status: "notApplicable", message: Option.none() },
      }),
      queryError: null,
    });

    expect(presentation.banners).toEqual([]);
    expect(presentation.availability).toBe("available");
  });
});
