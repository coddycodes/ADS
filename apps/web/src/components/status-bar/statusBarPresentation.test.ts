import type {
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
} from "@coddy/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { describe, expect, it } from "vite-plus/test";

import {
  buildResourceSummaryViewModel,
  environmentNotConnectedReason,
} from "./statusBarPresentation";

const updatedAt = DateTime.makeUnsafe("2026-07-07T18:00:00.000Z");

const mebibytes = (value: number): number => value * 1024 ** 2;

function processEntry(
  patch: Partial<ServerProcessDiagnosticsEntry> &
    Pick<ServerProcessDiagnosticsEntry, "processKey" | "pid" | "scope" | "kind" | "label">,
): ServerProcessDiagnosticsEntry {
  return {
    ppid: 1,
    pgid: Option.none(),
    status: "Run",
    cpuPercent: 1,
    rssBytes: mebibytes(50),
    elapsed: "00:00:01",
    command: `command-for-${patch.pid}`,
    depth: 0,
    childPids: [],
    confidence: "exact",
    ...patch,
  };
}

const resourceProcesses: ReadonlyArray<ServerProcessDiagnosticsEntry> = [
  processEntry({
    processKey: "100:1",
    pid: 100,
    scope: "core",
    kind: "server",
    label: "Coddy Studio Server",
    command: "coddy-studio server",
    rssBytes: mebibytes(250),
    cpuPercent: 2,
  }),
  processEntry({
    processKey: "101:1",
    pid: 101,
    scope: "core",
    kind: "ui",
    label: "Coddy Studio UI",
    command: "Coddy Studio WebContent",
    rssBytes: mebibytes(150),
    cpuPercent: 1,
  }),
  processEntry({
    processKey: "200:1",
    pid: 200,
    scope: "external",
    kind: "provider",
    label: "Codex",
    command: "codex app-server",
    rssBytes: mebibytes(150),
    cpuPercent: 4,
  }),
  processEntry({
    processKey: "201:1",
    pid: 201,
    scope: "external",
    kind: "terminal",
    label: "Build terminal",
    command: "pnpm test --filter web",
    rssBytes: mebibytes(100),
    cpuPercent: 2,
  }),
  processEntry({
    processKey: "202:1",
    pid: 202,
    scope: "external",
    kind: "unknown",
    label: "Unattributed process",
    command: "helper --serve",
    rssBytes: mebibytes(50),
    cpuPercent: 1,
    confidence: "fallback",
  }),
];

function diagnosticsFixture(
  patch: Partial<ServerProcessDiagnosticsResult> = {},
): ServerProcessDiagnosticsResult {
  return {
    serverPid: 100,
    readAt: updatedAt,
    totals: {
      combined: { processCount: 5, rssBytes: mebibytes(700), cpuPercent: 10 },
      core: { processCount: 2, rssBytes: mebibytes(400), cpuPercent: 3 },
      external: { processCount: 3, rssBytes: mebibytes(300), cpuPercent: 7 },
    },
    uiCoverage: { status: "available", message: Option.none() },
    processes: [...resourceProcesses],
    error: Option.none(),
    ...patch,
  };
}

function buildResourcePresentation(
  diagnostics: ServerProcessDiagnosticsResult | null,
  options: {
    readonly queryError?: string | null;
    readonly localDiagnostics?: ServerProcessDiagnosticsResult | null;
    readonly localQueryError?: string | null;
  } = {},
) {
  return buildResourceSummaryViewModel({
    selected: {
      diagnostics,
      queryError: options.queryError ?? null,
    },
    local:
      options.localDiagnostics === undefined && options.localQueryError === undefined
        ? null
        : {
            diagnostics: options.localDiagnostics ?? null,
            queryError: options.localQueryError ?? null,
          },
  });
}

describe("statusBarPresentation", () => {
  it("uses Combined for the headline and separates Core from External totals", () => {
    const vm = buildResourcePresentation(diagnosticsFixture());

    expect(vm.headline).toMatchObject({
      memoryLabel: "700.0 MB",
      cpuLabel: "10.0%",
      processCountLabel: "5",
    });
    expect(vm.core).toMatchObject({
      memoryLabel: "400.0 MB",
      cpuLabel: "3.0%",
      processCountLabel: "2",
    });
    expect(vm.external).toMatchObject({
      memoryLabel: "300.0 MB",
      cpuLabel: "7.0%",
      processCountLabel: "3",
    });
  });

  it("presents non-finite live CPU values as unavailable without failing reconciliation", () => {
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        totals: {
          combined: { processCount: 1, rssBytes: mebibytes(50), cpuPercent: Number.NaN },
          core: { processCount: 0, rssBytes: 0, cpuPercent: Number.POSITIVE_INFINITY },
          external: {
            processCount: 1,
            rssBytes: mebibytes(50),
            cpuPercent: Number.NEGATIVE_INFINITY,
          },
        },
        processes: [
          processEntry({
            processKey: "1:1",
            pid: 1,
            scope: "external",
            kind: "helper",
            label: "Invalid CPU sample",
            cpuPercent: Number.NaN,
          }),
        ],
      }),
    );

    expect(vm.headline?.cpuLabel).toBe("Unavailable");
    expect(vm.core?.cpuLabel).toBe("Unavailable");
    expect(vm.external?.cpuLabel).toBe("Unavailable");
    expect(vm.consumers[0]?.cpuLabel).toBe("Unavailable");
  });

  it("orders highest consumers by RSS and shows scope, memory, and CPU for every row", () => {
    const vm = buildResourcePresentation(diagnosticsFixture());

    expect(vm.consumers.map((consumer) => consumer.label)).toEqual([
      "Coddy Studio Server",
      "Coddy Studio UI",
      "Codex",
      "Build terminal",
      "Unattributed process",
    ]);
    expect(
      vm.consumers.map(({ scopeLabel, memoryLabel, cpuLabel }) => ({
        scopeLabel,
        memoryLabel,
        cpuLabel,
      })),
    ).toEqual([
      { scopeLabel: "Core", memoryLabel: "250.0 MB", cpuLabel: "2.0%" },
      { scopeLabel: "Core", memoryLabel: "150.0 MB", cpuLabel: "1.0%" },
      { scopeLabel: "External", memoryLabel: "150.0 MB", cpuLabel: "4.0%" },
      { scopeLabel: "External", memoryLabel: "100.0 MB", cpuLabel: "2.0%" },
      { scopeLabel: "External", memoryLabel: "50.0 MB", cpuLabel: "1.0%" },
    ]);
  });

  it("breaks equal-memory ties by label and then process key", () => {
    const tied = [
      processEntry({
        processKey: "3:1",
        pid: 3,
        scope: "external",
        kind: "helper",
        label: "Zulu",
      }),
      processEntry({
        processKey: "2:1",
        pid: 2,
        scope: "external",
        kind: "helper",
        label: "Alpha",
      }),
      processEntry({
        processKey: "1:1",
        pid: 1,
        scope: "external",
        kind: "helper",
        label: "Alpha",
      }),
    ];
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        totals: {
          combined: { processCount: 3, rssBytes: mebibytes(150), cpuPercent: 3 },
          core: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
          external: { processCount: 3, rssBytes: mebibytes(150), cpuPercent: 3 },
        },
        processes: tied,
      }),
    );

    expect(vm.consumers.map((consumer) => consumer.processKey)).toEqual(["1:1", "2:1", "3:1"]);
  });

  it.each([
    ["partial", "Some UI processes could not be sampled."],
    ["unavailable", "UI process accounting is unavailable."],
  ] as const)("creates a warning for %s UI coverage", (status, message) => {
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        uiCoverage: { status, message: Option.some(message) },
      }),
    );

    expect(vm.warning?.message).toContain(message);
  });

  it("does not warn when UI coverage is not applicable", () => {
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        uiCoverage: { status: "notApplicable", message: Option.none() },
      }),
    );

    expect(vm.warning).toBeNull();
  });

  it("retains a stale last-good sample and reports the refresh failure", () => {
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        error: Option.some({ message: "Process refresh timed out." }),
      }),
    );

    expect(vm.headline?.memoryLabel).toBe("700.0 MB");
    expect(vm.core?.memoryLabel).toBe("400.0 MB");
    expect(vm.external?.memoryLabel).toBe("300.0 MB");
    expect(vm.warning?.message).toContain("Showing the last successful sample.");
    expect(vm.warning?.message).toContain("Process refresh timed out.");
    expect(vm.warning?.statusLabel).toBe("Showing stale resource data");
  });

  it("reports an initial selected query failure as unavailable", () => {
    const vm = buildResourcePresentation(null, {
      queryError: "Selected diagnostics request failed.",
    });

    expect(vm.headline).toBeNull();
    expect(vm.core).toBeNull();
    expect(vm.external).toBeNull();
    expect(vm.warning).toEqual({
      message: "Selected diagnostics request failed.",
      statusLabel: "Resource data unavailable",
    });
  });

  it("retains selected query data and marks it stale when the query layer fails", () => {
    const vm = buildResourcePresentation(diagnosticsFixture(), {
      queryError: "Selected diagnostics connection was lost.",
    });

    expect(vm.headline?.memoryLabel).toBe("700.0 MB");
    expect(vm.warning).toEqual({
      message: "Showing the last successful sample. Selected diagnostics connection was lost.",
      statusLabel: "Showing stale resource data",
    });
  });

  it("uses unavailable presentation instead of healthy zeroes without a good sample", () => {
    const vm = buildResourcePresentation(
      diagnosticsFixture({
        totals: {
          combined: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
          core: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
          external: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
        },
        processes: [],
        error: Option.some({ message: "No process sample is available." }),
      }),
    );

    expect(vm.headline).toBeNull();
    expect(vm.core).toBeNull();
    expect(vm.external).toBeNull();
    expect(vm.consumers).toEqual([]);
    expect(vm.warning?.message).toContain("No process sample is available.");
  });

  it("keeps local Core separate from the selected remote-host totals", () => {
    const vm = buildResourcePresentation(diagnosticsFixture(), {
      localDiagnostics: diagnosticsFixture({
        totals: {
          combined: { processCount: 4, rssBytes: mebibytes(1_000), cpuPercent: 16 },
          core: { processCount: 2, rssBytes: mebibytes(900), cpuPercent: 12 },
          external: { processCount: 2, rssBytes: mebibytes(100), cpuPercent: 4 },
        },
        uiCoverage: {
          status: "unavailable",
          message: Option.some("This device UI usage is unavailable."),
        },
      }),
    });

    expect(vm.headline?.memoryLabel).toBe("700.0 MB");
    expect(vm.core?.memoryLabel).toBe("400.0 MB");
    expect(vm.external?.memoryLabel).toBe("300.0 MB");
    expect(vm.warning).toBeNull();
    expect(vm.localCore?.totals).toMatchObject({
      memoryLabel: "900.0 MB",
      cpuLabel: "12.0%",
      processCountLabel: "2",
      coverageLabel: "UI unavailable",
    });
    expect(vm.localCore?.warning?.message).toContain("This device UI usage is unavailable.");
  });

  it("renders an initial local query failure as unavailable without contaminating selected state", () => {
    const vm = buildResourcePresentation(diagnosticsFixture(), {
      localQueryError: "This device diagnostics request failed.",
    });

    expect(vm.headline?.memoryLabel).toBe("700.0 MB");
    expect(vm.warning).toBeNull();
    expect(vm.localCore?.totals).toBeNull();
    expect(vm.localCore?.warning).toEqual({
      message: "This device diagnostics request failed.",
      statusLabel: "Resource data unavailable",
    });
  });

  it("retains local query data with an independent stale warning", () => {
    const vm = buildResourcePresentation(diagnosticsFixture(), {
      localDiagnostics: diagnosticsFixture(),
      localQueryError: "This device diagnostics connection was lost.",
    });

    expect(vm.warning).toBeNull();
    expect(vm.localCore?.totals?.memoryLabel).toBe("400.0 MB");
    expect(vm.localCore?.warning).toEqual({
      message: "Showing the last successful sample. This device diagnostics connection was lost.",
      statusLabel: "Showing stale resource data",
    });
  });

  it("retains a local structured last-good sample with an independent stale warning", () => {
    const vm = buildResourcePresentation(diagnosticsFixture(), {
      localDiagnostics: diagnosticsFixture({
        error: Option.some({ message: "This device process refresh timed out." }),
      }),
    });

    expect(vm.warning).toBeNull();
    expect(vm.localCore?.totals?.memoryLabel).toBe("400.0 MB");
    expect(vm.localCore?.warning).toEqual({
      message: "Showing the last successful sample. This device process refresh timed out.",
      statusLabel: "Showing stale resource data",
    });
  });

  it("rejects selected-host totals that do not reconcile", () => {
    expect(() =>
      buildResourcePresentation(
        diagnosticsFixture({
          totals: {
            combined: { processCount: 99, rssBytes: mebibytes(700), cpuPercent: 10 },
            core: { processCount: 2, rssBytes: mebibytes(400), cpuPercent: 3 },
            external: { processCount: 3, rssBytes: mebibytes(300), cpuPercent: 7 },
          },
        }),
      ),
    ).toThrow(/reconcile/i);
  });
});

describe("environmentNotConnectedReason", () => {
  const environment = (label: string, phase: string) =>
    ({
      label,
      connection: { phase, error: null, traceId: null },
    }) as Parameters<typeof environmentNotConnectedReason>[0];

  it.each(["available", "offline", "reconnecting", "error"])(
    "names the selected environment when its connection is %s",
    (phase) => {
      expect(environmentNotConnectedReason(environment("macBook-16", phase))).toBe(
        "macBook-16 is not connected.",
      );
    },
  );

  it.each(["connected", "connecting"])("names no reason while %s", (phase) => {
    expect(environmentNotConnectedReason(environment("Local", phase))).toBeNull();
  });

  it("names no reason without a selected environment and falls back for a blank label", () => {
    expect(environmentNotConnectedReason(null)).toBeNull();
    expect(environmentNotConnectedReason(environment("  ", "offline"))).toBe(
      "The selected server is not connected.",
    );
  });

  it("turns the reason into an unavailable warning instead of a silent empty headline", () => {
    const presentation = buildResourceSummaryViewModel({
      selected: { diagnostics: null, queryError: "macBook-16 is not connected." },
      local: null,
    });
    expect(presentation.headline).toBeNull();
    expect(presentation.warning).toEqual({
      message: "macBook-16 is not connected.",
      statusLabel: "Resource data unavailable",
    });
  });
});
