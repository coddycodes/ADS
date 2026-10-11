import {
  EnvironmentId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ProviderInstanceConfig,
  type ResolvedKeybindingsConfig,
  type ServerProcessDiagnosticsEntry,
  type ServerProcessDiagnosticsResult,
  type ServerProcessResourceHistoryResult,
  type ServerProcessResourceHistorySummary,
  type ServerProvider,
  type ServerProviderModel,
  type ServerTraceDiagnosticsResult,
} from "@bibcode/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";

import type {
  ProviderUsageViewModel,
  UsageWindowViewModel,
} from "../../src/components/status-bar/providerUsagePresentation";

export const fixtureEnvironmentId = EnvironmentId.make("typography-settings-owned");
export const fixtureThreadRef = {
  environmentId: fixtureEnvironmentId,
  threadId: ThreadId.make("typography-preview-owned"),
};
export const fixtureInstanceId = ProviderInstanceId.make("codex_typography");
export const fixtureDriver = ProviderDriverKind.make("codex");
export const readAt = DateTime.makeUnsafe("2026-10-10T12:00:00.000Z");
const nextReadAt = DateTime.makeUnsafe("2026-10-10T12:01:00.000Z");
const MiB = 1024 ** 2;

export function processEntry(
  patch: Pick<ServerProcessDiagnosticsEntry, "pid" | "processKey" | "scope" | "kind" | "label"> &
    Partial<ServerProcessDiagnosticsEntry>,
): ServerProcessDiagnosticsEntry {
  return {
    ppid: 100,
    pgid: Option.none(),
    status: "Run",
    cpuPercent: 1,
    rssBytes: 10 * MiB,
    elapsed: "00:00:01",
    command: `fixture-command-for-${patch.pid}`,
    depth: 1,
    childPids: [],
    confidence: "exact",
    ...patch,
  };
}

// Values follow the current ResourceDiagnosticsSections.test.tsx factories.
export const resourceProcesses: ServerProcessDiagnosticsResult = {
  serverPid: 100,
  readAt,
  totals: {
    combined: { processCount: 3, rssBytes: 650 * MiB, cpuPercent: 16 },
    core: { processCount: 2, rssBytes: 320 * MiB, cpuPercent: 3 },
    external: { processCount: 1, rssBytes: 330 * MiB, cpuPercent: 13 },
  },
  uiCoverage: { status: "available", message: Option.none() },
  processes: [
    processEntry({
      pid: 100,
      ppid: 1,
      processKey: "100:1",
      scope: "core",
      kind: "server",
      label: "BiBCode Server",
      command: "bibcode server",
      rssBytes: 200 * MiB,
      cpuPercent: 2,
      depth: 0,
      childPids: [200, 300],
    }),
    processEntry({
      pid: 200,
      processKey: "200:1",
      scope: "external",
      kind: "provider",
      label: "Codex fixture worker",
      command: "codex app-server",
      rssBytes: 330 * MiB,
      cpuPercent: 13,
    }),
    processEntry({
      pid: 300,
      processKey: "300:1",
      scope: "core",
      kind: "ui",
      label: "BiBCode UI",
      command: "BiBCode WebContent",
      rssBytes: 120 * MiB,
    }),
  ],
  error: Option.none(),
};

function historyProcess(
  patch: Partial<ServerProcessResourceHistorySummary> = {},
): ServerProcessResourceHistorySummary {
  return {
    processKey: "100:1",
    pid: 100,
    ppid: 1,
    command: "bibcode server",
    depth: 0,
    scope: "core",
    kind: "server",
    label: "BiBCode Server",
    confidence: "exact",
    firstSeenAt: readAt,
    lastSeenAt: nextReadAt,
    currentCpuPercent: 2,
    avgCpuPercent: 1.5,
    maxCpuPercent: 8,
    cpuSecondsApprox: 65,
    currentRssBytes: 100 * MiB,
    maxRssBytes: 200 * MiB,
    sampleCount: 10,
    ...patch,
  };
}

export const resourceHistory: ServerProcessResourceHistoryResult = {
  readAt: nextReadAt,
  windowMs: 15 * 60_000,
  bucketMs: 60_000,
  sampleIntervalMs: 2_000,
  retainedSampleCount: 120,
  cpuSecondsApprox: { combined: 75, core: 65, external: 10 },
  uiCoverage: { status: "available", message: Option.none() },
  buckets: [
    {
      startedAt: readAt,
      endedAt: nextReadAt,
      cpuPercent: {
        average: { combined: 10, core: 6, external: 4 },
        peak: { combined: 40, core: 25, external: 15 },
      },
      rssBytes: {
        average: { combined: 800 * MiB, core: 500 * MiB, external: 300 * MiB },
        peak: { combined: 1024 * MiB, core: 600 * MiB, external: 424 * MiB },
      },
      maxProcessCount: { combined: 3, core: 2, external: 1 },
    },
  ],
  processes: [
    historyProcess(),
    historyProcess({
      processKey: "200:1",
      pid: 200,
      ppid: 100,
      scope: "external",
      kind: "provider",
      label: "Codex fixture worker",
      command: "codex app-server",
      depth: 1,
      cpuSecondsApprox: 10,
      currentCpuPercent: 13,
      avgCpuPercent: 7,
      maxCpuPercent: 40,
      currentRssBytes: 330 * MiB,
      maxRssBytes: 400 * MiB,
    }),
  ],
  error: Option.none(),
};

export const providerModels: ReadonlyArray<ServerProviderModel> = [
  {
    slug: "gpt-6.1-sol",
    name: "Sol fixture model",
    isCustom: false,
    capabilities: {
      optionDescriptors: [
        { id: "fastMode", label: "Fast mode", type: "boolean" },
        { id: "thinking", label: "Thinking", type: "boolean" },
        {
          id: "reasoningEffort",
          label: "Reasoning effort",
          type: "select",
          options: [{ id: "high", label: "High" }],
        },
      ],
    },
  },
  {
    slug: "hidden-fixture-model",
    name: "Hidden fixture model",
    isCustom: false,
    capabilities: null,
  },
  { slug: "owned-custom-model", name: "Custom fixture model", isCustom: true, capabilities: null },
];

export const providerInstance: ProviderInstanceConfig = {
  driver: fixtureDriver,
  displayName: "Typography provider instance",
  enabled: true,
  config: { binaryPath: "/owned-fixture/bin/codex", customModels: ["owned-custom-model"] },
  environment: [
    { name: "FIXTURE_API_BASE", value: "https://example.invalid", sensitive: false },
    { name: "FIXTURE_STORED_SECRET", value: "", sensitive: true, valueRedacted: true },
  ],
};

export const liveProvider: ServerProvider = {
  instanceId: fixtureInstanceId,
  driver: fixtureDriver,
  enabled: true,
  installed: true,
  version: "1.2.3",
  status: "ready",
  auth: {
    status: "authenticated",
    label: "Fixture subscription",
    email: "typography@example.invalid",
  },
  checkedAt: "2026-10-10T12:00:00.000Z",
  models: providerModels,
  slashCommands: [],
  skills: [],
  agents: [],
};

export const usageWindow: UsageWindowViewModel = {
  key: "session",
  label: "Session",
  windowLabel: "5h",
  consumedPercent: 72,
  displayedPercent: 28,
  fillPercent: 28,
  percentageLabel: "28% remaining",
  resetLabel: "Resets in 5m",
  resetsAt: null,
  resetDescription: null,
  barColorClass: "bg-warning",
};

export const usageViewModel: ProviderUsageViewModel = {
  provider: "codex",
  status: "ok",
  windows: [usageWindow],
  detailedWindows: [usageWindow],
  compactWindows: [usageWindow],
  plan: { value: "fixture", label: "Fixture" },
  credits: {
    availableCount: 1,
    totalEarnedCount: 1,
    nextExpiresAt: null,
    nextExpiresLabel: "in 1d",
  },
  updatedAt: readAt,
  error: null,
};

export const keybindings: ResolvedKeybindingsConfig = [
  {
    command: "terminal.split",
    shortcut: {
      key: "d",
      metaKey: false,
      ctrlKey: false,
      altKey: true,
      shiftKey: false,
      modKey: true,
    },
    whenAst: {
      type: "and",
      left: { type: "identifier", name: "terminalFocus" },
      right: {
        type: "or",
        left: { type: "not", node: { type: "identifier", name: "composerFocus" } },
        right: { type: "identifier", name: "threadFocus" },
      },
    },
  },
];

export const longCause =
  "Fixture request failed while decoding the supplied component data.\n" +
  "This deliberately long owned diagnostic message exceeds one hundred and eighty characters so that the real ExpandableText control exposes its full-error button without replacing the changed visual child.";
export const longTraceId = "trace-0123456789abcdefghijklmnopqrstuvwxyz-9876";
export const traceDiagnostics: ServerTraceDiagnosticsResult = {
  traceFilePath: "/owned-fixture/logs/trace.jsonl",
  scannedFilePaths: ["/owned-fixture/logs/trace.jsonl"],
  readAt,
  recordCount: 1234,
  parseErrorCount: 2,
  firstSpanAt: Option.some(readAt),
  lastSpanAt: Option.some(readAt),
  failureCount: 3,
  interruptionCount: 0,
  slowSpanThresholdMs: 500,
  slowSpanCount: 4,
  logLevelCounts: { warn: 1 },
  topSpansByCount: [
    {
      name: "fixture.trace.request",
      count: 10,
      failureCount: 1,
      totalDurationMs: 1000,
      averageDurationMs: 100,
      maxDurationMs: 1200,
    },
  ],
  slowestSpans: [
    {
      name: "fixture.trace.slow",
      durationMs: 12000,
      endedAt: readAt,
      traceId: longTraceId,
      spanId: "fixture-slow",
    },
  ],
  commonFailures: [
    {
      name: "fixture.trace.error",
      cause: "Owned fixture decode failure",
      count: 5,
      lastSeenAt: readAt,
      traceId: "fixture-trace",
      spanId: "fixture-error",
    },
  ],
  latestFailures: [
    {
      name: "fixture.trace.failed",
      cause: longCause,
      durationMs: 9000,
      endedAt: readAt,
      traceId: "fixture-trace",
      spanId: "fixture-failed",
    },
  ],
  latestWarningAndErrorLogs: [
    {
      spanName: "fixture.trace.log",
      level: "warn",
      message: longCause,
      seenAt: readAt,
      traceId: longTraceId,
      spanId: "fixture-log",
    },
  ],
  partialFailure: Option.none(),
  error: Option.none(),
};
