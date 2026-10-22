import type { ServerProviderUsageWindow } from "@bibcode/contracts";
import * as DateTime from "effect/DateTime";

const NUMBER_FORMAT = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 1,
  minimumFractionDigits: 1,
});

export function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, value));
}

export function formatProviderUsagePercent(
  consumedPercent: number,
  display: "remaining" | "used",
): string {
  const consumed = clampPercent(consumedPercent);
  const percent = display === "remaining" ? 100 - consumed : consumed;
  return `${Math.round(percent)}% ${display}`;
}

function formatCountdown(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "now";
  const seconds = Math.floor(milliseconds / 1_000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

export function formatProviderResetLabel(
  resetsAt: DateTime.Utc | null,
  resetDescription: string | null,
  now: DateTime.Utc,
): string | null {
  if (resetsAt !== null) {
    return `Resets in ${formatCountdown(
      DateTime.toEpochMillis(resetsAt) - DateTime.toEpochMillis(now),
    )}`;
  }
  return resetDescription;
}

export function formatProviderWindowLabel(
  window: Pick<ServerProviderUsageWindow, "windowMinutes"> & { readonly resetsAt?: unknown },
): string {
  if (!Number.isFinite(window.windowMinutes) || window.windowMinutes < 0) return "--";
  if (window.windowMinutes === 300) return "5h";
  if (window.windowMinutes === 10080) return "wk";
  if (window.windowMinutes >= 43200 && window.windowMinutes <= 44640) return "mo";
  if (window.windowMinutes % 60 === 0) return `${window.windowMinutes / 60}h`;
  return `${window.windowMinutes}m`;
}

export function providerUsageBarColorClass(usedPercent: number): string {
  const remaining = 100 - clampPercent(usedPercent);
  if (remaining > 40) return "bg-emerald-500";
  if (remaining > 20) return "bg-yellow-500";
  return "bg-red-500";
}

/**
 * What a process's memory figure (`rssBytes`) measures, which the server's OS decides
 * (#159). Shown wherever Resource Manager reports memory.
 */
export const PROCESS_MEMORY_METRIC_NOTE =
  "Memory is the physical footprint on macOS, as in Activity Monitor, and resident memory on Linux and Windows.";

export function formatMemoryBytes(bytes: number): string {
  const safeBytes = Math.max(0, bytes);
  if (safeBytes >= 1024 ** 3) {
    return `${NUMBER_FORMAT.format(safeBytes / 1024 ** 3)} GB`;
  }
  if (safeBytes >= 1024 ** 2) {
    return `${NUMBER_FORMAT.format(safeBytes / 1024 ** 2)} MB`;
  }
  if (safeBytes >= 1024) {
    return `${NUMBER_FORMAT.format(safeBytes / 1024)} KB`;
  }
  return `${Math.round(safeBytes)} B`;
}

export function formatCpuPercent(percent: number): string {
  if (!Number.isFinite(percent)) return "Unavailable";
  return `${NUMBER_FORMAT.format(Math.max(0, percent))}%`;
}
