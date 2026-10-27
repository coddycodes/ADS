import type {
  ServerProviderUsageResetCredits,
  ServerProviderUsageSnapshot,
  ServerProviderUsageWindow,
} from "@coddy/contracts";
import * as DateTime from "effect/DateTime";

import {
  clampPercent,
  formatProviderResetLabel,
  formatProviderUsagePercent,
  formatProviderWindowLabel,
  providerUsageBarColorClass,
} from "./statusBarFormat";

export type UsageWindowKey = "session" | "weekly" | "fable";
export type UsagePercentageDisplay = "remaining" | "used";

export interface UsageWindowViewModel {
  readonly key: UsageWindowKey;
  readonly label: "Session" | "Weekly" | "Fable";
  readonly windowLabel: string;
  readonly consumedPercent: number;
  readonly displayedPercent: number;
  readonly fillPercent: number;
  readonly percentageLabel: string;
  readonly resetLabel: string | null;
  readonly resetsAt: ServerProviderUsageWindow["resetsAt"];
  readonly resetDescription: string | null;
  /** Color intentionally continues to reflect consumed urgency in both display modes. */
  readonly barColorClass: string;
}

export interface ProviderUsagePlanViewModel {
  readonly value: string;
  readonly label: string;
}

export interface ProviderUsageCreditsViewModel {
  readonly availableCount: number;
  readonly totalEarnedCount: number | null;
  readonly nextExpiresAt: ServerProviderUsageResetCredits["nextExpiresAt"];
  readonly nextExpiresLabel: string | null;
}

export interface ProviderUsageViewModel {
  readonly provider: ServerProviderUsageSnapshot["provider"];
  readonly status: ServerProviderUsageSnapshot["status"];
  readonly windows: ReadonlyArray<UsageWindowViewModel>;
  readonly detailedWindows: ReadonlyArray<UsageWindowViewModel>;
  readonly compactWindows: ReadonlyArray<UsageWindowViewModel>;
  readonly plan: ProviderUsagePlanViewModel | null;
  readonly credits: ProviderUsageCreditsViewModel | null;
  readonly updatedAt: ServerProviderUsageSnapshot["updatedAt"];
  readonly error: string | null;
}

export interface ProviderUsagePresentationOptions {
  readonly now?: DateTime.Utc;
  readonly percentageDisplay?: UsagePercentageDisplay;
}

const WINDOW_DEFINITIONS: ReadonlyArray<{
  readonly key: UsageWindowKey;
  readonly label: UsageWindowViewModel["label"];
  readonly snapshotKey: "session" | "weekly" | "fableWeekly";
}> = [
  { key: "session", label: "Session", snapshotKey: "session" },
  { key: "weekly", label: "Weekly", snapshotKey: "weekly" },
  { key: "fable", label: "Fable", snapshotKey: "fableWeekly" },
];

function titleCasePlan(value: string): string {
  return value
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map((word) => `${word.slice(0, 1).toUpperCase()}${word.slice(1).toLowerCase()}`)
    .join(" ");
}

function buildWindowViewModel(input: {
  readonly key: UsageWindowKey;
  readonly label: UsageWindowViewModel["label"];
  readonly window: ServerProviderUsageWindow;
  readonly now: DateTime.Utc;
  readonly percentageDisplay: UsagePercentageDisplay;
}): UsageWindowViewModel {
  const consumedPercent = clampPercent(input.window.usedPercent);
  const displayedPercent =
    input.percentageDisplay === "remaining" ? 100 - consumedPercent : consumedPercent;

  return {
    key: input.key,
    label: input.label,
    windowLabel: formatProviderWindowLabel(input.window),
    consumedPercent,
    displayedPercent,
    fillPercent: displayedPercent,
    percentageLabel: formatProviderUsagePercent(consumedPercent, input.percentageDisplay),
    resetLabel: formatProviderResetLabel(
      input.window.resetsAt,
      input.window.resetDescription,
      input.now,
    ),
    resetsAt: input.window.resetsAt,
    resetDescription: input.window.resetDescription,
    barColorClass: providerUsageBarColorClass(consumedPercent),
  };
}

function buildCreditsViewModel(
  credits: ServerProviderUsageResetCredits | null,
  now: DateTime.Utc,
): ProviderUsageCreditsViewModel | null {
  if (credits === null) return null;
  return {
    availableCount: credits.availableCount,
    totalEarnedCount: credits.totalEarnedCount,
    nextExpiresAt: credits.nextExpiresAt,
    nextExpiresLabel:
      credits.nextExpiresAt === null
        ? null
        : formatProviderResetLabel(credits.nextExpiresAt, null, now),
  };
}

export function buildProviderUsageViewModel(
  snapshot: ServerProviderUsageSnapshot,
  options: ProviderUsagePresentationOptions = {},
): ProviderUsageViewModel {
  const now = options.now ?? DateTime.nowUnsafe();
  const percentageDisplay = options.percentageDisplay ?? "remaining";
  const windows = WINDOW_DEFINITIONS.flatMap((definition) => {
    const window = snapshot[definition.snapshotKey];
    return window === null
      ? []
      : [
          buildWindowViewModel({
            ...definition,
            window,
            now,
            percentageDisplay,
          }),
        ];
  });
  const tightestWindow = windows.reduce<UsageWindowViewModel | null>(
    (tightest, window) =>
      tightest === null || window.consumedPercent > tightest.consumedPercent ? window : tightest,
    null,
  );
  const plan =
    snapshot.planType === null
      ? null
      : (() => {
          const label = titleCasePlan(snapshot.planType);
          return label.length === 0 ? null : { value: snapshot.planType, label };
        })();

  return {
    provider: snapshot.provider,
    status: snapshot.status,
    windows,
    detailedWindows: windows,
    compactWindows: tightestWindow === null ? [] : [tightestWindow],
    plan,
    credits: buildCreditsViewModel(snapshot.rateLimitResetCredits, now),
    updatedAt: snapshot.updatedAt,
    error: snapshot.error,
  };
}

export function buildProviderUsageViewModels(
  snapshots: ReadonlyArray<ServerProviderUsageSnapshot>,
  options: ProviderUsagePresentationOptions = {},
): ReadonlyArray<ProviderUsageViewModel> {
  return snapshots.map((snapshot) => buildProviderUsageViewModel(snapshot, options));
}

export function providerUsageRelativeLabelKey(
  snapshots: ReadonlyArray<ServerProviderUsageSnapshot>,
  now: DateTime.Utc,
): string {
  return JSON.stringify(
    snapshots.flatMap((snapshot) => [
      ...WINDOW_DEFINITIONS.map(({ snapshotKey }) => {
        const window = snapshot[snapshotKey];
        return window === null || window.resetsAt === null
          ? null
          : formatProviderResetLabel(window.resetsAt, null, now);
      }),
      snapshot.rateLimitResetCredits?.nextExpiresAt === null ||
      snapshot.rateLimitResetCredits?.nextExpiresAt === undefined
        ? null
        : formatProviderResetLabel(snapshot.rateLimitResetCredits.nextExpiresAt, null, now),
    ]),
  );
}
