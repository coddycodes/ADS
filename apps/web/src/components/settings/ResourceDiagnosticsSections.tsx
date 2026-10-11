import { useMemo, useState, type ReactNode } from "react";
import type {
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerProcessSignal,
} from "@bibcode/contracts";

import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { ScrollArea } from "../ui/scroll-area";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { SettingsSection } from "./settingsLayout";
import {
  DEFAULT_HISTORY_PROCESS_SORT,
  DEFAULT_LIVE_PROCESS_SORT,
  HISTORY_PROCESS_COLUMNS,
  LIVE_PROCESS_COLUMNS,
  RESOURCE_HISTORY_WINDOWS,
  presentLiveProcesses,
  presentResourceHistory,
  toggleHistoryProcessSort,
  toggleLiveProcessSort,
  type HistoryMetric,
  type HistoryProcessSort,
  type HistoryProcessSortKey,
  type LiveProcessSort,
  type LiveProcessSortKey,
  type ResourceDiagnosticsBanner,
  type ResourceHistorySummaryPresentation,
  type ResourceTotalsPresentation,
} from "./resourceDiagnosticsPresentation";

export interface ResourceDiagnosticsSectionsProps {
  readonly processData: ServerProcessDiagnosticsResult | null;
  readonly processError: string | null;
  readonly isProcessPending: boolean;
  readonly signalingPid: number | null;
  readonly supportsInterrupt: boolean;
  readonly onSignal: (pid: number, processKey: string, signal: ServerProcessSignal) => void;
  readonly liveHeaderAction: ReactNode;
  readonly resourceData: ServerProcessResourceHistoryResult | null;
  readonly resourceError: string | null;
  readonly isResourcePending: boolean;
  readonly resourceWindowMs: number;
  readonly onSelectResourceWindow: (windowMs: number) => void;
  readonly historyHeaderAction: ReactNode;
}

function SummaryMetric({ label, value }: { readonly label: string; readonly value: string }) {
  return (
    <div className="min-w-0">
      <div className="text-xs font-medium uppercase tracking-[0.08em] text-muted-foreground">
        {label}
      </div>
      <div className="mt-0.5 truncate font-mono text-sm font-semibold tabular-nums text-foreground">
        {value}
      </div>
    </div>
  );
}

function LiveSummaryCard({
  scope,
  summary,
  headline = false,
}: {
  readonly scope: "combined" | "core" | "external";
  readonly summary: ResourceTotalsPresentation;
  readonly headline?: boolean;
}) {
  return (
    <div
      data-resource-card={scope}
      className={cn("min-w-0 px-4 py-3 sm:px-5", headline ? "border-b border-border/60" : "flex-1")}
    >
      <div className="text-xs font-semibold text-foreground">{summary.title}</div>
      <div className="mt-2 grid grid-cols-3 gap-4">
        <SummaryMetric label="Memory" value={summary.memoryLabel} />
        <SummaryMetric label="CPU" value={summary.cpuLabel} />
        <SummaryMetric label="Processes" value={summary.processCountLabel} />
      </div>
    </div>
  );
}

function HistorySummaryCard({
  scope,
  summary,
  headline = false,
}: {
  readonly scope: "combined" | "core" | "external";
  readonly summary: ResourceHistorySummaryPresentation;
  readonly headline?: boolean;
}) {
  return (
    <div
      data-resource-card={scope}
      className={cn("min-w-0 px-4 py-3 sm:px-5", headline ? "border-b border-border/60" : "flex-1")}
    >
      <div className="text-xs font-semibold text-foreground">{summary.title}</div>
      <SummaryMetric label="Approx. CPU time" value={summary.valueLabel} />
    </div>
  );
}

function ResourceSummaryPair({ children }: { readonly children: ReactNode }) {
  return (
    <div
      data-resource-card-pair="true"
      className="grid grid-cols-1 divide-y divide-border/60 sm:grid-cols-2 sm:divide-x sm:divide-y-0"
    >
      {children}
    </div>
  );
}

function ResourceBanners({
  banners,
}: {
  readonly banners: ReadonlyArray<ResourceDiagnosticsBanner>;
}) {
  if (banners.length === 0) return null;
  return (
    <div className="space-y-2 border-t border-border/60 px-4 py-3 sm:px-5">
      {banners.map((banner) => (
        <div
          key={`${banner.statusLabel}:${banner.message}`}
          role={banner.tone === "danger" ? "alert" : "status"}
          className={cn(
            "rounded-md border px-3 py-2 text-xs",
            banner.tone === "danger"
              ? "border-destructive/30 bg-destructive/5 text-destructive"
              : "border-amber-500/30 bg-amber-500/5 text-amber-700 dark:text-amber-300",
          )}
        >
          <span className="font-semibold">{banner.statusLabel}</span>
          <span> — {banner.message}</span>
        </div>
      ))}
    </div>
  );
}

function SortHeader({
  label,
  sortLabel = label,
  sortKey,
  sort,
  onSort,
  align = "left",
}: {
  readonly label: string;
  readonly sortLabel?: string;
  readonly sortKey: LiveProcessSortKey;
  readonly sort: LiveProcessSort;
  readonly onSort: (key: LiveProcessSortKey) => void;
  readonly align?: "left" | "right";
}) {
  const active = sort.key === sortKey;
  return (
    <th
      aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
      className={cn("px-3 py-2 font-semibold", align === "right" && "text-right")}
    >
      <Button
        type="button"
        variant="ghost"
        size="xs"
        className="-mx-2 h-6 px-2 text-xs font-semibold uppercase tracking-[0.08em]"
        aria-label={`Sort by ${sortLabel}`}
        onClick={() => onSort(sortKey)}
      >
        {label}
        {active ? <span aria-hidden>{sort.direction === "asc" ? " ↑" : " ↓"}</span> : null}
      </Button>
    </th>
  );
}

function HistorySortHeader({
  label,
  sortKey,
  sort,
  onSort,
  align = "left",
}: {
  readonly label: string;
  readonly sortKey: HistoryProcessSortKey;
  readonly sort: HistoryProcessSort;
  readonly onSort: (key: HistoryProcessSortKey) => void;
  readonly align?: "left" | "right";
}) {
  const active = sort.key === sortKey;
  return (
    <th
      aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
      className={cn("whitespace-nowrap px-3 py-2 font-semibold", align === "right" && "text-right")}
    >
      <Button
        type="button"
        variant="ghost"
        size="xs"
        className="-mx-2 h-6 px-2 text-xs font-semibold uppercase tracking-[0.08em]"
        aria-label={`Sort history by ${label}`}
        onClick={() => onSort(sortKey)}
      >
        {label}
        {active ? <span aria-hidden>{sort.direction === "asc" ? " ↑" : " ↓"}</span> : null}
      </Button>
    </th>
  );
}

function LiveProcessTable({
  presentation,
  isPending,
  signalingPid,
  supportsInterrupt,
  onSignal,
  onSort,
}: {
  readonly presentation: ReturnType<typeof presentLiveProcesses>;
  readonly isPending: boolean;
  readonly signalingPid: number | null;
  readonly supportsInterrupt: boolean;
  readonly onSignal: ResourceDiagnosticsSectionsProps["onSignal"];
  readonly onSort: (key: LiveProcessSortKey) => void;
}) {
  return (
    <ScrollArea
      chainVerticalScroll
      scrollFade
      hideScrollbars
      className="max-h-[min(64vh,44rem)] w-full max-w-full border-t border-border/60"
    >
      <table className="w-full min-w-[1120px] table-fixed text-left text-xs">
        <colgroup>
          <col className="w-[9%]" />
          <col className="w-[9%]" />
          <col className="w-[17%]" />
          <col className="w-[30%]" />
          <col className="w-[9%]" />
          <col className="w-[11%]" />
          <col className="w-[7%]" />
          <col className="w-[8%]" />
        </colgroup>
        <thead className="sticky top-0 z-10 border-b border-border/60 bg-card text-xs uppercase tracking-[0.08em] text-muted-foreground">
          <tr>
            <SortHeader
              label={LIVE_PROCESS_COLUMNS[0]}
              sortKey="scope"
              sort={presentation.sort}
              onSort={onSort}
            />
            <th className="px-3 py-2 font-semibold">{LIVE_PROCESS_COLUMNS[1]}</th>
            <SortHeader
              label={LIVE_PROCESS_COLUMNS[2]}
              sortLabel="Name"
              sortKey="name"
              sort={presentation.sort}
              onSort={onSort}
            />
            <th className="px-3 py-2 font-semibold">{LIVE_PROCESS_COLUMNS[3]}</th>
            <SortHeader
              label={LIVE_PROCESS_COLUMNS[4]}
              sortKey="cpu"
              sort={presentation.sort}
              onSort={onSort}
              align="right"
            />
            <SortHeader
              label={LIVE_PROCESS_COLUMNS[5]}
              sortKey="memory"
              sort={presentation.sort}
              onSort={onSort}
              align="right"
            />
            <th className="px-3 py-2 text-right font-semibold">{LIVE_PROCESS_COLUMNS[6]}</th>
            <th className="px-3 py-2 text-right font-semibold">Signal</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border/50">
          {presentation.rows.length === 0 ? (
            <tr>
              <td colSpan={8} className="px-4 py-4 text-xs text-muted-foreground sm:px-5">
                {isPending
                  ? "Loading live processes..."
                  : presentation.availability === "unavailable"
                    ? "Live process data is unavailable."
                    : "No live processes found."}
              </td>
            </tr>
          ) : null}
          {presentation.rows.map((row) => (
            <tr key={row.processKey} className="hover:bg-muted/20">
              <td className="px-3 py-2">
                <span
                  className={cn(
                    "rounded-sm px-1.5 py-0.5 text-xs font-semibold uppercase",
                    row.scope === "core"
                      ? "bg-blue-500/10 text-blue-700 dark:text-blue-300"
                      : "bg-violet-500/10 text-violet-700 dark:text-violet-300",
                  )}
                >
                  {row.scopeLabel}
                </span>
              </td>
              <td className="px-3 py-2 text-muted-foreground">{row.kindLabel}</td>
              <td className="px-3 py-2">
                <span className="block truncate font-medium text-foreground">{row.label}</span>
              </td>
              <td className="px-3 py-2 text-muted-foreground">
                <Tooltip>
                  <TooltipTrigger render={<span className="block truncate">{row.command}</span>} />
                  <TooltipPopup
                    side="top"
                    className="max-w-[min(440px,calc(100vw-2rem))] break-words font-mono text-xs"
                  >
                    {row.command}
                  </TooltipPopup>
                </Tooltip>
              </td>
              <td className="px-3 py-2 text-right font-mono tabular-nums">{row.cpuLabel}</td>
              <td className="px-3 py-2 text-right font-mono tabular-nums">{row.memoryLabel}</td>
              <td className="px-3 py-2 text-right font-mono tabular-nums text-muted-foreground">
                {row.pid}
              </td>
              <td className="px-3 py-2">
                {row.canSignal ? (
                  <div className="flex items-center justify-end gap-1">
                    {supportsInterrupt ? (
                      <Button
                        type="button"
                        variant="ghost"
                        size="xs"
                        disabled={signalingPid === row.pid}
                        aria-label={`Send SIGINT to ${row.label}`}
                        onClick={() => onSignal(row.pid, row.processKey, "SIGINT")}
                      >
                        INT
                      </Button>
                    ) : null}
                    <Button
                      type="button"
                      variant="ghost"
                      size="xs"
                      className="text-destructive"
                      disabled={signalingPid === row.pid}
                      aria-label={`Send SIGKILL to ${row.label}`}
                      onClick={() => onSignal(row.pid, row.processKey, "SIGKILL")}
                    >
                      KILL
                    </Button>
                  </div>
                ) : (
                  <span
                    className="block text-right text-muted-foreground"
                    aria-label="Not signalable"
                  >
                    —
                  </span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </ScrollArea>
  );
}

function ResourceHistoryWindowSelector({
  selectedWindowMs,
  onSelect,
}: {
  readonly selectedWindowMs: number;
  readonly onSelect: (windowMs: number) => void;
}) {
  return (
    <div className="flex items-center rounded-md border border-border/60 p-0.5">
      {RESOURCE_HISTORY_WINDOWS.map((option) => (
        <Button
          key={option.windowMs}
          type="button"
          variant="ghost"
          size="xs"
          className={cn(
            "h-6 rounded-sm px-2 text-xs font-medium text-muted-foreground",
            selectedWindowMs === option.windowMs && "bg-muted text-foreground",
          )}
          aria-label={`Show ${option.label} resource history`}
          aria-pressed={selectedWindowMs === option.windowMs}
          onClick={() => onSelect(option.windowMs)}
        >
          {option.label}
        </Button>
      ))}
    </div>
  );
}

function HistoryMetricToggle({
  metric,
  onChange,
}: {
  readonly metric: HistoryMetric;
  readonly onChange: (metric: HistoryMetric) => void;
}) {
  return (
    <div className="flex items-center rounded-md border border-border/60 p-0.5">
      {(["memory", "cpu"] as const).map((option) => {
        const label = option === "memory" ? "Memory" : "CPU";
        return (
          <Button
            key={option}
            type="button"
            variant="ghost"
            size="xs"
            className={cn(
              "h-6 rounded-sm px-2 text-xs font-medium text-muted-foreground",
              metric === option && "bg-muted text-foreground",
            )}
            aria-label={`${label} history`}
            aria-pressed={metric === option}
            onClick={() => onChange(option)}
          >
            {label}
          </Button>
        );
      })}
    </div>
  );
}

function chartPercentage(value: number): number {
  return Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
}

function ResourceHistoryChart({
  presentation,
}: {
  readonly presentation: ReturnType<typeof presentResourceHistory>;
}) {
  if (presentation.chart.bars.length === 0) return null;
  return (
    <div className="border-t border-border/60 px-4 py-3 sm:px-5">
      <div className="flex h-32 items-end gap-1 overflow-hidden rounded-sm bg-muted/10 p-2">
        {presentation.chart.bars.map((bar) => {
          const combined = Math.max(0, bar.average.combined);
          const totalHeight = Math.max(
            2,
            chartPercentage((combined / presentation.chart.maximumAverage) * 100),
          );
          const coreRatio =
            combined === 0 ? 0 : chartPercentage((bar.average.core / combined) * 100);
          const externalRatio =
            combined === 0 ? 0 : chartPercentage((bar.average.external / combined) * 100);
          return (
            <Tooltip key={bar.key}>
              <TooltipTrigger
                render={
                  <div className="flex h-full min-w-1 flex-1 items-end" aria-label={bar.tooltip}>
                    <div
                      className="flex w-full flex-col-reverse overflow-hidden rounded-t-sm"
                      style={{ height: `${totalHeight}%` }}
                    >
                      <div
                        data-history-stack="core"
                        className="w-full bg-blue-500/70"
                        style={{ height: `${coreRatio}%` }}
                      />
                      <div
                        data-history-stack="external"
                        className="w-full bg-violet-500/70"
                        style={{ height: `${externalRatio}%` }}
                      />
                    </div>
                  </div>
                }
              />
              <TooltipPopup side="top" className="max-w-xs text-left text-xs">
                {bar.tooltip}
              </TooltipPopup>
            </Tooltip>
          );
        })}
      </div>
      <div className="mt-2 flex items-center gap-4 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1">
          <span className="size-2 rounded-sm bg-blue-500/70" aria-hidden />
          BiBCode Core average
        </span>
        <span className="inline-flex items-center gap-1">
          <span className="size-2 rounded-sm bg-violet-500/70" aria-hidden />
          External Tooling average
        </span>
      </div>
    </div>
  );
}

function HistoryProcessTable({
  presentation,
  isPending,
  onSort,
}: {
  readonly presentation: ReturnType<typeof presentResourceHistory>;
  readonly isPending: boolean;
  readonly onSort: (key: HistoryProcessSortKey) => void;
}) {
  return (
    <ScrollArea
      chainVerticalScroll
      scrollFade
      hideScrollbars
      className="max-h-[min(64vh,44rem)] w-full max-w-full border-t border-border/60"
    >
      <table className="w-full min-w-[1480px] table-fixed text-left text-xs">
        <thead className="sticky top-0 z-10 border-b border-border/60 bg-card text-xs uppercase tracking-[0.08em] text-muted-foreground">
          <tr>
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[0]}
              sortKey="scope"
              sort={presentation.processSort}
              onSort={onSort}
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[1]}
              sortKey="kind"
              sort={presentation.processSort}
              onSort={onSort}
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[2]}
              sortKey="label"
              sort={presentation.processSort}
              onSort={onSort}
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[3]}
              sortKey="cpuTime"
              sort={presentation.processSort}
              onSort={onSort}
              align="right"
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[4]}
              sortKey="currentCpu"
              sort={presentation.processSort}
              onSort={onSort}
              align="right"
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[5]}
              sortKey="averageCpu"
              sort={presentation.processSort}
              onSort={onSort}
              align="right"
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[6]}
              sortKey="peakCpu"
              sort={presentation.processSort}
              onSort={onSort}
              align="right"
            />
            <HistorySortHeader
              label={HISTORY_PROCESS_COLUMNS[7]}
              sortKey="maxMemory"
              sort={presentation.processSort}
              onSort={onSort}
              align="right"
            />
            <th className="whitespace-nowrap px-3 py-2 font-semibold">
              {HISTORY_PROCESS_COLUMNS[8]}
            </th>
            <th className="whitespace-nowrap px-3 py-2 text-right font-semibold">
              {HISTORY_PROCESS_COLUMNS[9]}
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border/50">
          {presentation.rows.length === 0 ? (
            <tr>
              <td
                colSpan={HISTORY_PROCESS_COLUMNS.length}
                className="px-4 py-4 text-xs text-muted-foreground sm:px-5"
              >
                {isPending
                  ? "Collecting process resource samples..."
                  : presentation.availability === "unavailable"
                    ? "Resource history is unavailable."
                    : "No process resource samples found for this window."}
              </td>
            </tr>
          ) : null}
          {presentation.rows.map((row) => (
            <tr key={row.processKey} className="hover:bg-muted/20">
              <td className="px-3 py-2">
                <span
                  className={cn(
                    "rounded-sm px-1.5 py-0.5 text-xs font-semibold uppercase",
                    row.scope === "core"
                      ? "bg-blue-500/10 text-blue-700 dark:text-blue-300"
                      : "bg-violet-500/10 text-violet-700 dark:text-violet-300",
                  )}
                >
                  {row.scopeLabel}
                </span>
              </td>
              <td className="px-3 py-2 text-muted-foreground">{row.kindLabel}</td>
              <td className="px-3 py-2">
                <span className="block truncate font-medium text-foreground">{row.label}</span>
              </td>
              {[
                ["cpu-time", row.cpuTimeLabel],
                ["current-cpu", row.currentCpuLabel],
                ["average-cpu", row.averageCpuLabel],
                ["peak-cpu", row.peakCpuLabel],
                ["max-memory", row.maxMemoryLabel],
              ].map(([metric, value]) => (
                <td
                  key={`${row.processKey}:${metric}`}
                  className="px-3 py-2 text-right font-mono tabular-nums"
                >
                  {value}
                </td>
              ))}
              <td className="px-3 py-2 text-muted-foreground">
                <Tooltip>
                  <TooltipTrigger render={<span className="block truncate">{row.command}</span>} />
                  <TooltipPopup
                    side="top"
                    className="max-w-[min(440px,calc(100vw-2rem))] break-words font-mono text-xs"
                  >
                    {row.command}
                  </TooltipPopup>
                </Tooltip>
              </td>
              <td className="px-3 py-2 text-right font-mono tabular-nums text-muted-foreground">
                {row.pid}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </ScrollArea>
  );
}

export function ResourceDiagnosticsSections({
  processData,
  processError,
  isProcessPending,
  signalingPid,
  supportsInterrupt,
  onSignal,
  liveHeaderAction,
  resourceData,
  resourceError,
  isResourcePending,
  resourceWindowMs,
  onSelectResourceWindow,
  historyHeaderAction,
}: ResourceDiagnosticsSectionsProps) {
  const [liveSort, setLiveSort] = useState<LiveProcessSort>(DEFAULT_LIVE_PROCESS_SORT);
  const [historyMetric, setHistoryMetric] = useState<HistoryMetric>("memory");
  const [historyProcessSort, setHistoryProcessSort] = useState<HistoryProcessSort>(
    DEFAULT_HISTORY_PROCESS_SORT,
  );
  const live = useMemo(
    () =>
      presentLiveProcesses({
        diagnostics: processData,
        queryError: processError,
        sort: liveSort,
      }),
    [liveSort, processData, processError],
  );
  const history = useMemo(
    () =>
      presentResourceHistory({
        history: resourceData,
        queryError: resourceError,
        metric: historyMetric,
        processSort: historyProcessSort,
      }),
    [historyMetric, historyProcessSort, resourceData, resourceError],
  );
  const sortLiveProcesses = (key: LiveProcessSortKey) => {
    setLiveSort((current) => toggleLiveProcessSort(current, key));
  };
  const sortHistoryProcesses = (key: HistoryProcessSortKey) => {
    setHistoryProcessSort((current) => toggleHistoryProcessSort(current, key));
  };

  return (
    <>
      <SettingsSection title="Live Processes" headerAction={liveHeaderAction}>
        {live.summary ? (
          <>
            <LiveSummaryCard scope="combined" summary={live.summary.combined} headline />
            <ResourceSummaryPair>
              <LiveSummaryCard scope="core" summary={live.summary.core} />
              <LiveSummaryCard scope="external" summary={live.summary.external} />
            </ResourceSummaryPair>
            <div className="border-t border-border/60 px-4 py-2 text-xs text-muted-foreground sm:px-5">
              Server PID <span className="font-mono tabular-nums">{processData?.serverPid}</span>
            </div>
          </>
        ) : (
          <div className="px-4 py-4 text-xs text-muted-foreground sm:px-5">
            Combined, Core, and External totals are unavailable.
          </div>
        )}
        <ResourceBanners banners={live.banners} />
        <LiveProcessTable
          presentation={live}
          isPending={isProcessPending}
          signalingPid={signalingPid}
          supportsInterrupt={supportsInterrupt}
          onSignal={onSignal}
          onSort={sortLiveProcesses}
        />
      </SettingsSection>

      <SettingsSection
        title="Resource History"
        headerAction={
          <div className="flex flex-wrap items-center justify-end gap-1.5">
            <ResourceHistoryWindowSelector
              selectedWindowMs={resourceWindowMs}
              onSelect={onSelectResourceWindow}
            />
            {historyHeaderAction}
          </div>
        }
      >
        <div className="flex items-center justify-between gap-3 px-4 py-2 sm:px-5">
          <div className="text-xs text-muted-foreground">
            {history.sampleCountLabel} samples · {history.sampleIntervalLabel} interval ·{" "}
            {history.processCountLabel} processes
          </div>
          <HistoryMetricToggle metric={history.metric} onChange={setHistoryMetric} />
        </div>
        {history.summary ? (
          <>
            <HistorySummaryCard scope="combined" summary={history.summary.combined} headline />
            <ResourceSummaryPair>
              <HistorySummaryCard scope="core" summary={history.summary.core} />
              <HistorySummaryCard scope="external" summary={history.summary.external} />
            </ResourceSummaryPair>
          </>
        ) : (
          <div className="border-t border-border/60 px-4 py-4 text-xs text-muted-foreground sm:px-5">
            Combined, Core, and External history is unavailable.
          </div>
        )}
        <ResourceBanners banners={history.banners} />
        <ResourceHistoryChart presentation={history} />
        <HistoryProcessTable
          presentation={history}
          isPending={isResourcePending}
          onSort={sortHistoryProcesses}
        />
      </SettingsSection>
    </>
  );
}
