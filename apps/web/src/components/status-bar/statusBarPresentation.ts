import type { EnvironmentConnectionPresentation } from "@bibcode/client-runtime/connection";
import type {
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceTotals,
  ServerProcessUiCoverage,
} from "@bibcode/contracts";
import * as Option from "effect/Option";

import { formatCpuPercent, formatMemoryBytes } from "./statusBarFormat";

export interface ResourceTotalsPresentation {
  readonly memoryLabel: string;
  readonly cpuLabel: string;
  readonly processCountLabel: string;
  readonly coverageLabel: string | null;
}

export interface ResourceConsumerPresentation {
  readonly processKey: string;
  readonly scope: ServerProcessDiagnosticsEntry["scope"];
  readonly scopeLabel: "Core" | "External";
  readonly label: string;
  readonly command: string;
  readonly memoryLabel: string;
  readonly cpuLabel: string;
}

export interface ResourceWarningPresentation {
  readonly message: string;
  readonly statusLabel:
    | "Resource data unavailable"
    | "Showing stale resource data"
    | "Resource data warning";
}

export interface ResourceDiagnosticsQueryState {
  readonly diagnostics: ServerProcessDiagnosticsResult | null;
  readonly queryError: string | null;
}

export interface LocalCoreResourcePresentation {
  readonly totals: ResourceTotalsPresentation | null;
  readonly warning: ResourceWarningPresentation | null;
}

export interface ResourceUsagePresentation {
  readonly headline: ResourceTotalsPresentation | null;
  readonly core: ResourceTotalsPresentation | null;
  readonly external: ResourceTotalsPresentation | null;
  readonly consumers: ReadonlyArray<ResourceConsumerPresentation>;
  readonly uiCoverage: ServerProcessUiCoverage;
  readonly localCore: LocalCoreResourcePresentation | null;
  readonly warning: ResourceWarningPresentation | null;
}

const UNAVAILABLE_UI_COVERAGE: ServerProcessUiCoverage = {
  status: "unavailable",
  message: Option.none(),
};

const HIGHEST_CONSUMER_LIMIT = 5;

function coverageLabel(coverage: ServerProcessUiCoverage): string {
  switch (coverage.status) {
    case "available":
      return "UI coverage available";
    case "partial":
      return "UI coverage partial";
    case "unavailable":
      return "UI unavailable";
    case "notApplicable":
      return "UI not applicable";
  }
}

function presentTotals(
  totals: ServerProcessResourceTotals,
  coverage: ServerProcessUiCoverage | null = null,
): ResourceTotalsPresentation {
  return {
    memoryLabel: formatMemoryBytes(totals.rssBytes),
    cpuLabel: formatCpuPercent(totals.cpuPercent),
    processCountLabel: String(totals.processCount),
    coverageLabel: coverage === null ? null : coverageLabel(coverage),
  };
}

/**
 * Why the selected environment's readouts are empty: its connection is down. A first
 * attempt still in progress is not a failure, so it names no reason.
 */
export function environmentNotConnectedReason(
  environment: {
    readonly label: string;
    readonly connection: EnvironmentConnectionPresentation;
  } | null,
): string | null {
  if (environment === null) return null;
  const { phase } = environment.connection;
  if (phase === "connected" || phase === "connecting") return null;
  const label = environment.label.trim();
  return `${label.length > 0 ? label : "The selected server"} is not connected.`;
}

function compareText(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function selectHighestConsumers(
  processes: ReadonlyArray<ServerProcessDiagnosticsEntry>,
): ReadonlyArray<ServerProcessDiagnosticsEntry> {
  return processes.toSorted(
    (left, right) =>
      right.rssBytes - left.rssBytes ||
      compareText(left.label, right.label) ||
      compareText(left.processKey, right.processKey),
  );
}

function presentConsumer(process: ServerProcessDiagnosticsEntry): ResourceConsumerPresentation {
  return {
    processKey: process.processKey,
    scope: process.scope,
    scopeLabel: process.scope === "core" ? "Core" : "External",
    label: process.label,
    command: process.command,
    memoryLabel: formatMemoryBytes(process.rssBytes),
    cpuLabel: formatCpuPercent(process.cpuPercent),
  };
}

function totalsReconcile(input: ServerProcessDiagnosticsResult["totals"]): boolean {
  if (
    input.combined.rssBytes !== input.core.rssBytes + input.external.rssBytes ||
    input.combined.processCount !== input.core.processCount + input.external.processCount
  ) {
    return false;
  }
  if (
    !Number.isFinite(input.combined.cpuPercent) ||
    !Number.isFinite(input.core.cpuPercent) ||
    !Number.isFinite(input.external.cpuPercent)
  ) {
    return true;
  }
  const expectedCpuPercent = input.core.cpuPercent + input.external.cpuPercent;
  const cpuTolerance = Math.max(1, Math.abs(expectedCpuPercent)) * 1e-9;
  return Math.abs(input.combined.cpuPercent - expectedCpuPercent) <= cpuTolerance;
}

function assertReconciledTotals(input: ServerProcessDiagnosticsResult["totals"]): void {
  if (!totalsReconcile(input)) {
    throw new Error("Combined resource totals must reconcile with Core plus External totals.");
  }
}

function coverageWarning(coverage: ServerProcessUiCoverage): string | null {
  if (coverage.status !== "partial" && coverage.status !== "unavailable") return null;
  return (
    Option.getOrNull(coverage.message) ??
    (coverage.status === "partial"
      ? "BiBCode UI resource coverage is partial; Core and Combined totals may be incomplete."
      : "BiBCode UI resource coverage is unavailable; unobserved UI usage is not included.")
  );
}

function isGoodSample(diagnostics: ServerProcessDiagnosticsResult): boolean {
  const hasError = Option.isSome(diagnostics.error);
  return !(
    hasError &&
    diagnostics.totals.combined.processCount === 0 &&
    diagnostics.processes.length === 0
  );
}

function buildResourceWarning(input: {
  readonly diagnostics: ServerProcessDiagnosticsResult | null;
  readonly queryError: string | null;
  readonly hasGoodSample: boolean;
}): ResourceWarningPresentation | null {
  const diagnosticError =
    input.diagnostics === null ? null : Option.getOrNull(input.diagnostics.error)?.message;
  const errorMessages = [diagnosticError, input.queryError].filter(
    (message): message is string => typeof message === "string",
  );
  const uniqueErrorMessages = [...new Set(errorMessages)];
  const errorMessage =
    uniqueErrorMessages.length === 0
      ? null
      : `${input.hasGoodSample ? "Showing the last successful sample. " : ""}${uniqueErrorMessages.join(" ")}`;
  const coverageMessage =
    input.diagnostics === null ? null : coverageWarning(input.diagnostics.uiCoverage);
  const messages = [errorMessage, coverageMessage].filter(
    (message): message is string => message !== null,
  );
  if (messages.length === 0) return null;
  return {
    message: messages.join(" "),
    statusLabel:
      uniqueErrorMessages.length === 0
        ? "Resource data warning"
        : input.hasGoodSample
          ? "Showing stale resource data"
          : "Resource data unavailable",
  };
}

export function buildResourceSummaryViewModel(input: {
  readonly selected: ResourceDiagnosticsQueryState;
  readonly local: ResourceDiagnosticsQueryState | null;
}): ResourceUsagePresentation {
  const diagnostics = input.selected.diagnostics;
  const localDiagnostics = input.local?.diagnostics ?? null;
  const uiCoverage = diagnostics?.uiCoverage ?? UNAVAILABLE_UI_COVERAGE;
  const hasGoodSample = diagnostics !== null && isGoodSample(diagnostics);
  const hasGoodLocalSample = localDiagnostics !== null && isGoodSample(localDiagnostics);

  if (diagnostics !== null) assertReconciledTotals(diagnostics.totals);
  if (localDiagnostics !== null) assertReconciledTotals(localDiagnostics.totals);

  return {
    headline: hasGoodSample ? presentTotals(diagnostics.totals.combined) : null,
    core: hasGoodSample ? presentTotals(diagnostics.totals.core) : null,
    external: hasGoodSample ? presentTotals(diagnostics.totals.external) : null,
    consumers: hasGoodSample
      ? selectHighestConsumers(diagnostics.processes)
          .slice(0, HIGHEST_CONSUMER_LIMIT)
          .map(presentConsumer)
      : [],
    uiCoverage,
    localCore:
      input.local === null
        ? null
        : {
            totals: hasGoodLocalSample
              ? presentTotals(localDiagnostics.totals.core, localDiagnostics.uiCoverage)
              : null,
            warning: buildResourceWarning({
              diagnostics: localDiagnostics,
              queryError: input.local.queryError,
              hasGoodSample: hasGoodLocalSample,
            }),
          },
    warning: buildResourceWarning({
      diagnostics,
      queryError: input.selected.queryError,
      hasGoodSample,
    }),
  };
}
