import type {
  ServerProcessAttributionKind,
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerProcessResourceHistorySummary,
  ServerProcessResourceTotals,
  ServerProcessUiCoverage,
} from "@coddy/contracts";
import * as Option from "effect/Option";

import { formatCpuPercent, formatMemoryBytes } from "../status-bar/statusBarFormat";

export type LiveProcessSortKey = "memory" | "cpu" | "name" | "scope";
export type LiveProcessSortDirection = "asc" | "desc";
export type HistoryProcessSortKey =
  | "label"
  | "scope"
  | "kind"
  | "cpuTime"
  | "currentCpu"
  | "averageCpu"
  | "peakCpu"
  | "maxMemory";
export type HistoryMetric = "memory" | "cpu";

export interface LiveProcessSort {
  readonly key: LiveProcessSortKey;
  readonly direction: LiveProcessSortDirection;
}

export interface HistoryProcessSort {
  readonly key: HistoryProcessSortKey;
  readonly direction: LiveProcessSortDirection;
}

export const DEFAULT_LIVE_PROCESS_SORT: LiveProcessSort = {
  key: "memory",
  direction: "desc",
};

export const DEFAULT_HISTORY_PROCESS_SORT: HistoryProcessSort = {
  key: "maxMemory",
  direction: "desc",
};

export const LIVE_PROCESS_COLUMNS = [
  "Scope",
  "Kind",
  "Label",
  "Command",
  "CPU",
  "Memory",
  "PID",
] as const;

export const HISTORY_PROCESS_COLUMNS = [
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
] as const;

export const RESOURCE_HISTORY_WINDOWS = [
  { label: "5m", windowMs: 5 * 60_000, bucketMs: 30_000 },
  { label: "15m", windowMs: 15 * 60_000, bucketMs: 60_000 },
  { label: "30m", windowMs: 30 * 60_000, bucketMs: 2 * 60_000 },
  { label: "1h", windowMs: 60 * 60_000, bucketMs: 5 * 60_000 },
] as const;

export interface ResourceDiagnosticsBanner {
  readonly tone: "warning" | "danger";
  readonly statusLabel:
    | "Showing stale resource data"
    | "Resource data unavailable"
    | "Partial UI coverage"
    | "UI coverage unavailable";
  readonly message: string;
}

export interface ResourceTotalsPresentation {
  readonly title: "Combined" | "Coddy Studio Core" | "External Tooling";
  readonly memoryLabel: string;
  readonly cpuLabel: string;
  readonly processCountLabel: string;
}

export interface LiveProcessRowPresentation {
  readonly processKey: string;
  readonly pid: number;
  readonly scope: ServerProcessDiagnosticsEntry["scope"];
  readonly scopeLabel: "Core" | "External";
  readonly kind: ServerProcessDiagnosticsEntry["kind"];
  readonly kindLabel: string;
  readonly label: string;
  readonly command: string;
  readonly cpuPercent: number;
  readonly cpuLabel: string;
  readonly rssBytes: number;
  readonly memoryLabel: string;
  readonly canSignal: boolean;
}

export interface LiveProcessesPresentation {
  readonly checkedAt: ServerProcessDiagnosticsResult["readAt"] | null;
  readonly availability: "available" | "stale" | "unavailable";
  readonly summary: {
    readonly combined: ResourceTotalsPresentation;
    readonly core: ResourceTotalsPresentation;
    readonly external: ResourceTotalsPresentation;
  } | null;
  readonly rows: ReadonlyArray<LiveProcessRowPresentation>;
  readonly sort: LiveProcessSort;
  readonly banners: ReadonlyArray<ResourceDiagnosticsBanner>;
}

export interface ResourceHistorySummaryPresentation {
  readonly title: "Combined" | "Coddy Studio Core" | "External Tooling";
  readonly valueLabel: string;
}

interface SplitMetric {
  readonly combined: number;
  readonly core: number;
  readonly external: number;
}

export interface ResourceHistoryBarPresentation {
  readonly key: string;
  readonly startedAt: ServerProcessResourceHistoryResult["readAt"];
  readonly endedAt: ServerProcessResourceHistoryResult["readAt"];
  readonly average: SplitMetric;
  readonly peak: SplitMetric;
  readonly averageLabels: {
    readonly combined: string;
    readonly core: string;
    readonly external: string;
  };
  readonly peakLabels: {
    readonly combined: string;
    readonly core: string;
    readonly external: string;
  };
  readonly tooltip: string;
}

export interface ResourceHistoryProcessRowPresentation {
  readonly processKey: string;
  readonly pid: number;
  readonly scope: ServerProcessResourceHistorySummary["scope"];
  readonly scopeLabel: "Core" | "External";
  readonly kind: ServerProcessResourceHistorySummary["kind"];
  readonly kindLabel: string;
  readonly label: string;
  readonly command: string;
  readonly cpuTimeLabel: string;
  readonly currentCpuLabel: string;
  readonly averageCpuLabel: string;
  readonly peakCpuLabel: string;
  readonly maxMemoryLabel: string;
}

export interface ResourceHistoryPresentation {
  readonly checkedAt: ServerProcessResourceHistoryResult["readAt"] | null;
  readonly availability: "available" | "stale" | "unavailable";
  readonly metric: HistoryMetric;
  readonly processSort: HistoryProcessSort;
  readonly summary: {
    readonly combined: ResourceHistorySummaryPresentation;
    readonly core: ResourceHistorySummaryPresentation;
    readonly external: ResourceHistorySummaryPresentation;
  } | null;
  readonly sampleCountLabel: string;
  readonly sampleIntervalLabel: string;
  readonly processCountLabel: string;
  readonly chart: {
    readonly bars: ReadonlyArray<ResourceHistoryBarPresentation>;
    readonly maximumAverage: number;
  };
  readonly rows: ReadonlyArray<ResourceHistoryProcessRowPresentation>;
  readonly banners: ReadonlyArray<ResourceDiagnosticsBanner>;
}

function compareText(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function numericSortRank(value: number): number {
  if (value === Number.NEGATIVE_INFINITY) return 0;
  if (Number.isFinite(value)) return 1;
  if (value === Number.POSITIVE_INFINITY) return 2;
  return 3;
}

function compareNumbersTotal(left: number, right: number): number {
  const rankDifference = numericSortRank(left) - numericSortRank(right);
  if (rankDifference !== 0) return rankDifference;
  if (!Number.isFinite(left)) return 0;
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function scopeLabel(scope: ServerProcessDiagnosticsEntry["scope"]): "Core" | "External" {
  return scope === "core" ? "Core" : "External";
}

function kindLabel(kind: ServerProcessAttributionKind): string {
  switch (kind) {
    case "server":
      return "Server";
    case "ui":
      return "UI";
    case "provider":
      return "Provider";
    case "terminal":
      return "Terminal";
    case "helper":
      return "Helper";
    case "unknown":
      return "Unknown";
  }
}

function formatCpuTime(seconds: number): string {
  if (!Number.isFinite(seconds)) return "Unavailable";
  if (seconds < 60) return `${seconds.toFixed(seconds >= 10 ? 1 : 2)}s`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${minutes.toFixed(minutes >= 10 ? 1 : 2)}m`;
  return `${(minutes / 60).toFixed(2)}h`;
}

function formatHistoryCpuPercent(percent: number): string {
  if (!Number.isFinite(percent)) return "Unavailable";
  return percent < 0 ? `-${formatCpuPercent(-percent)}` : formatCpuPercent(percent);
}

function formatHistoryMemoryBytes(bytes: number): string {
  return Number.isFinite(bytes) ? formatMemoryBytes(bytes) : "Unavailable";
}

function sanitizeHistoryMetric(metric: SplitMetric): SplitMetric {
  const sanitize = (value: number): number => (Number.isFinite(value) ? value : 0);
  return {
    combined: sanitize(metric.combined),
    core: sanitize(metric.core),
    external: sanitize(metric.external),
  };
}

function formatDuration(value: number): string {
  if (value < 1_000) return `${Math.round(value)} ms`;
  return `${(value / 1_000).toFixed(value >= 10_000 ? 1 : 2)} s`;
}

function boundMessage(message: string, maximumScalars = 320): string {
  const scalars = [...message];
  return scalars.length <= maximumScalars
    ? message
    : `${scalars.slice(0, maximumScalars - 1).join("")}…`;
}

function coverageBanner(coverage: ServerProcessUiCoverage): ResourceDiagnosticsBanner | null {
  if (coverage.status !== "partial" && coverage.status !== "unavailable") return null;
  const detail =
    Option.getOrNull(coverage.message) ??
    (coverage.status === "partial"
      ? "Local UI process coverage is partial."
      : "Local UI process coverage is unavailable.");
  return {
    tone: "warning",
    statusLabel: coverage.status === "partial" ? "Partial UI coverage" : "UI coverage unavailable",
    message: boundMessage(`${detail} Core and Combined totals omit unobserved UI resource usage.`),
  };
}

function errorBanners(input: {
  readonly diagnosticMessage: string | null;
  readonly queryError: string | null;
  readonly hasGoodSample: boolean;
}): ReadonlyArray<ResourceDiagnosticsBanner> {
  const messages = [input.diagnosticMessage, input.queryError]
    .filter((message): message is string => message !== null)
    .map((message) => boundMessage(message));
  const uniqueMessages = [...new Set(messages)];
  if (uniqueMessages.length === 0) return [];
  return [
    {
      tone: input.hasGoodSample ? "warning" : "danger",
      statusLabel: input.hasGoodSample
        ? "Showing stale resource data"
        : "Resource data unavailable",
      message: boundMessage(
        `${
          input.hasGoodSample ? "Showing the last successful server sample. " : ""
        }${uniqueMessages.join(" ")}`,
      ),
    },
  ];
}

function presentTotals(
  title: ResourceTotalsPresentation["title"],
  totals: ServerProcessResourceTotals,
): ResourceTotalsPresentation {
  return {
    title,
    memoryLabel: formatMemoryBytes(totals.rssBytes),
    cpuLabel: formatCpuPercent(totals.cpuPercent),
    processCountLabel: String(totals.processCount),
  };
}

function currentServerDescendantProcessKeys(
  processes: ReadonlyArray<ServerProcessDiagnosticsEntry>,
  serverPid: number,
): ReadonlySet<string> {
  const processesByPid = new Map<number, Array<ServerProcessDiagnosticsEntry>>();
  const processKeyCounts = new Map<string, number>();
  for (const process of processes) {
    const samePidProcesses = processesByPid.get(process.pid);
    if (samePidProcesses === undefined) {
      processesByPid.set(process.pid, [process]);
    } else {
      samePidProcesses.push(process);
    }
    processKeyCounts.set(process.processKey, (processKeyCounts.get(process.processKey) ?? 0) + 1);
  }

  if (processesByPid.get(serverPid)?.length !== 1) return new Set();

  const descendants = new Set<string>();
  for (const process of processes) {
    if (process.pid === serverPid) continue;
    if (processKeyCounts.get(process.processKey) !== 1) continue;
    if (processesByPid.get(process.pid)?.length !== 1) continue;

    let current: ServerProcessDiagnosticsEntry | undefined = process;
    const visited = new Set<number>();
    while (current !== undefined && !visited.has(current.pid)) {
      visited.add(current.pid);
      if (current.ppid === serverPid) {
        descendants.add(process.processKey);
        break;
      }
      const parents = processesByPid.get(current.ppid);
      if (parents?.length !== 1) break;
      current = parents[0];
    }
  }
  return descendants;
}

function compareLiveProcesses(
  left: ServerProcessDiagnosticsEntry,
  right: ServerProcessDiagnosticsEntry,
  sort: LiveProcessSort,
): number {
  let result: number;
  switch (sort.key) {
    case "memory":
      result = left.rssBytes - right.rssBytes;
      break;
    case "cpu":
      result = compareNumbersTotal(left.cpuPercent, right.cpuPercent);
      break;
    case "name":
      result = compareText(left.label, right.label);
      break;
    case "scope":
      result = compareText(left.scope, right.scope);
      break;
  }
  if (result !== 0) return sort.direction === "asc" ? result : -result;
  return compareText(left.processKey, right.processKey);
}

export function toggleLiveProcessSort(
  current: LiveProcessSort,
  key: LiveProcessSortKey,
): LiveProcessSort {
  if (current.key === key) {
    return {
      key,
      direction: current.direction === "asc" ? "desc" : "asc",
    };
  }
  return {
    key,
    direction: key === "memory" || key === "cpu" ? "desc" : "asc",
  };
}

export function toggleHistoryProcessSort(
  current: HistoryProcessSort,
  key: HistoryProcessSortKey,
): HistoryProcessSort {
  if (current.key === key) {
    return {
      key,
      direction: current.direction === "asc" ? "desc" : "asc",
    };
  }
  return {
    key,
    direction: key === "label" || key === "scope" || key === "kind" ? "asc" : "desc",
  };
}

export function presentLiveProcesses(input: {
  readonly diagnostics: ServerProcessDiagnosticsResult | null;
  readonly queryError: string | null;
  readonly sort?: LiveProcessSort;
}): LiveProcessesPresentation {
  const sort = input.sort ?? DEFAULT_LIVE_PROCESS_SORT;
  const diagnosticMessage =
    input.diagnostics === null
      ? null
      : (Option.getOrNull(input.diagnostics.error)?.message ?? null);
  const hasDiagnosticError = diagnosticMessage !== null;
  const hasGoodSample =
    input.diagnostics !== null &&
    !(
      hasDiagnosticError &&
      input.diagnostics.totals.combined.processCount === 0 &&
      input.diagnostics.processes.length === 0
    );
  const stale = hasGoodSample && (hasDiagnosticError || input.queryError !== null);
  const coverage = input.diagnostics === null ? null : coverageBanner(input.diagnostics.uiCoverage);
  const banners = [
    ...errorBanners({
      diagnosticMessage,
      queryError: input.queryError,
      hasGoodSample,
    }),
    ...(coverage === null ? [] : [coverage]),
  ];

  if (!hasGoodSample) {
    return {
      checkedAt: input.diagnostics?.readAt ?? null,
      availability: "unavailable",
      summary: null,
      rows: [],
      sort,
      banners,
    };
  }

  const diagnostics = input.diagnostics;
  const descendantProcessKeys = currentServerDescendantProcessKeys(
    diagnostics.processes,
    diagnostics.serverPid,
  );
  return {
    checkedAt: diagnostics.readAt,
    availability: stale ? "stale" : "available",
    summary: {
      combined: presentTotals("Combined", diagnostics.totals.combined),
      core: presentTotals("Coddy Studio Core", diagnostics.totals.core),
      external: presentTotals("External Tooling", diagnostics.totals.external),
    },
    rows: diagnostics.processes
      .toSorted((left, right) => compareLiveProcesses(left, right, sort))
      .map((process) => ({
        processKey: process.processKey,
        pid: process.pid,
        scope: process.scope,
        scopeLabel: scopeLabel(process.scope),
        kind: process.kind,
        kindLabel: kindLabel(process.kind),
        label: process.label,
        command: process.command,
        cpuPercent: process.cpuPercent,
        cpuLabel: formatCpuPercent(process.cpuPercent),
        rssBytes: process.rssBytes,
        memoryLabel: formatMemoryBytes(process.rssBytes),
        canSignal: process.scope === "external" && descendantProcessKeys.has(process.processKey),
      })),
    sort,
    banners,
  };
}

function historyMetric(
  history: ServerProcessResourceHistoryResult,
  metric: HistoryMetric,
): ReadonlyArray<ResourceHistoryBarPresentation> {
  return history.buckets.map((bucket) => {
    const values = metric === "memory" ? bucket.rssBytes : bucket.cpuPercent;
    const format = metric === "memory" ? formatHistoryMemoryBytes : formatHistoryCpuPercent;
    const averageLabels = {
      combined: format(values.average.combined),
      core: format(values.average.core),
      external: format(values.average.external),
    };
    const peakLabels = {
      combined: format(values.peak.combined),
      core: format(values.peak.core),
      external: format(values.peak.external),
    };
    return {
      key: `${bucket.startedAt}`,
      startedAt: bucket.startedAt,
      endedAt: bucket.endedAt,
      average: sanitizeHistoryMetric(values.average),
      peak: sanitizeHistoryMetric(values.peak),
      averageLabels,
      peakLabels,
      tooltip: `Combined average ${averageLabels.combined}. Same-sample peak ${peakLabels.combined}: Core ${peakLabels.core}, External ${peakLabels.external}.`,
    };
  });
}

function presentHistoryProcess(
  process: ServerProcessResourceHistorySummary,
): ResourceHistoryProcessRowPresentation {
  return {
    processKey: process.processKey,
    pid: process.pid,
    scope: process.scope,
    scopeLabel: scopeLabel(process.scope),
    kind: process.kind,
    kindLabel: kindLabel(process.kind),
    label: process.label,
    command: process.command,
    cpuTimeLabel: formatCpuTime(process.cpuSecondsApprox),
    currentCpuLabel: formatHistoryCpuPercent(process.currentCpuPercent),
    averageCpuLabel: formatHistoryCpuPercent(process.avgCpuPercent),
    peakCpuLabel: formatHistoryCpuPercent(process.maxCpuPercent),
    maxMemoryLabel: formatHistoryMemoryBytes(process.maxRssBytes),
  };
}

function compareHistoryProcesses(
  left: ServerProcessResourceHistorySummary,
  right: ServerProcessResourceHistorySummary,
  sort: HistoryProcessSort,
): number {
  let result: number;
  switch (sort.key) {
    case "label":
      result = compareText(left.label, right.label);
      break;
    case "scope":
      result = compareText(left.scope, right.scope);
      break;
    case "kind":
      result = compareText(left.kind, right.kind);
      break;
    case "cpuTime":
      result = compareNumbersTotal(left.cpuSecondsApprox, right.cpuSecondsApprox);
      break;
    case "currentCpu":
      result = compareNumbersTotal(left.currentCpuPercent, right.currentCpuPercent);
      break;
    case "averageCpu":
      result = compareNumbersTotal(left.avgCpuPercent, right.avgCpuPercent);
      break;
    case "peakCpu":
      result = compareNumbersTotal(left.maxCpuPercent, right.maxCpuPercent);
      break;
    case "maxMemory":
      result = compareNumbersTotal(left.maxRssBytes, right.maxRssBytes);
      break;
  }
  if (result !== 0) return sort.direction === "asc" ? result : -result;
  return compareText(left.processKey, right.processKey);
}

export function presentResourceHistory(input: {
  readonly history: ServerProcessResourceHistoryResult | null;
  readonly queryError: string | null;
  readonly metric?: HistoryMetric;
  readonly processSort?: HistoryProcessSort;
}): ResourceHistoryPresentation {
  const metric = input.metric ?? "memory";
  const processSort = input.processSort ?? DEFAULT_HISTORY_PROCESS_SORT;
  const diagnosticMessage =
    input.history === null ? null : (Option.getOrNull(input.history.error)?.message ?? null);
  const hasDiagnosticError = diagnosticMessage !== null;
  const hasGoodSample = input.history !== null && input.history.retainedSampleCount > 0;
  const stale = hasGoodSample && (hasDiagnosticError || input.queryError !== null);
  const coverage = input.history === null ? null : coverageBanner(input.history.uiCoverage);
  const banners = [
    ...errorBanners({
      diagnosticMessage,
      queryError: input.queryError,
      hasGoodSample,
    }),
    ...(coverage === null ? [] : [coverage]),
  ];
  const bars = hasGoodSample && input.history !== null ? historyMetric(input.history, metric) : [];

  if (!hasGoodSample) {
    return {
      checkedAt: input.history?.readAt ?? null,
      availability: "unavailable",
      metric,
      processSort,
      summary: null,
      sampleCountLabel: "...",
      sampleIntervalLabel: "...",
      processCountLabel: "...",
      chart: { bars, maximumAverage: 1 },
      rows: [],
      banners,
    };
  }

  const history = input.history;
  return {
    checkedAt: history.readAt,
    availability: stale ? "stale" : "available",
    metric,
    processSort,
    summary: {
      combined: {
        title: "Combined",
        valueLabel: formatCpuTime(history.cpuSecondsApprox.combined),
      },
      core: {
        title: "Coddy Studio Core",
        valueLabel: formatCpuTime(history.cpuSecondsApprox.core),
      },
      external: {
        title: "External Tooling",
        valueLabel: formatCpuTime(history.cpuSecondsApprox.external),
      },
    },
    sampleCountLabel: String(history.retainedSampleCount),
    sampleIntervalLabel: formatDuration(history.sampleIntervalMs),
    processCountLabel: String(history.processes.length),
    chart: {
      bars,
      maximumAverage: Math.max(1, ...bars.map((bar) => bar.average.combined)),
    },
    rows: history.processes
      .toSorted((left, right) => compareHistoryProcesses(left, right, processSort))
      .map(presentHistoryProcess),
    banners,
  };
}
