import { CpuIcon, MemoryStickIcon, TerminalIcon, TriangleAlertIcon } from "lucide-react";
import { useEffect, useRef } from "react";

import {
  acquireBrowserSurfaceOcclusion,
  type BrowserSurfaceOcclusionLease,
} from "~/browser/browserSurfaceStore";
import { isDesktopHost } from "~/env";
import { isMacPlatform } from "~/lib/utils";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/popover";
import {
  buildResourceSummaryViewModel,
  type ResourceDiagnosticsQueryState,
  type ResourceTotalsPresentation,
} from "./statusBarPresentation";

function ResourceTotalsCard({
  title,
  totals,
}: {
  readonly title: string;
  readonly totals: ResourceTotalsPresentation | null;
}) {
  return (
    <div className="min-w-0 rounded-md border border-border/70 bg-muted/20 p-2.5">
      <div className="font-medium text-foreground">{title}</div>
      {totals === null ? (
        <div className="mt-2 text-muted-foreground">Unavailable</div>
      ) : (
        <div className="mt-2 grid grid-cols-3 gap-2">
          <div className="min-w-0">
            <div className="text-xs text-muted-foreground">Memory</div>
            <div className="truncate font-mono tabular-nums">{totals.memoryLabel}</div>
          </div>
          <div className="min-w-0">
            <div className="text-xs text-muted-foreground">CPU</div>
            <div className="truncate font-mono tabular-nums">{totals.cpuLabel}</div>
          </div>
          <div className="min-w-0">
            <div className="text-xs text-muted-foreground">Processes</div>
            <div className="truncate font-mono tabular-nums">{totals.processCountLabel}</div>
          </div>
        </div>
      )}
    </div>
  );
}

export function ResourceUsageSegment({
  diagnostics,
  localDiagnostics,
  terminalCount,
  iconOnly,
}: {
  diagnostics: ResourceDiagnosticsQueryState;
  localDiagnostics: ResourceDiagnosticsQueryState | null;
  terminalCount: number;
  iconOnly: boolean;
}) {
  const previewOcclusionRef = useRef<BrowserSurfaceOcclusionLease | null>(null);
  const handleOpenChange = (open: boolean) => {
    if (
      open &&
      previewOcclusionRef.current === null &&
      isDesktopHost &&
      typeof navigator !== "undefined" &&
      isMacPlatform(navigator.platform)
    ) {
      previewOcclusionRef.current = acquireBrowserSurfaceOcclusion();
      return;
    }
    if (!open) {
      previewOcclusionRef.current?.release();
      previewOcclusionRef.current = null;
    }
  };
  useEffect(
    () => () => {
      previewOcclusionRef.current?.release();
      previewOcclusionRef.current = null;
    },
    [],
  );

  const presentation = buildResourceSummaryViewModel({
    selected: diagnostics,
    local: localDiagnostics,
  });
  const terminalCountLabel = String(Math.max(0, terminalCount));
  const headline = presentation.headline;
  const compactMemoryLabel = headline?.memoryLabel ?? "--";
  const compactCpuLabel = headline?.cpuLabel ?? "--";
  const selectedWarningLabel =
    presentation.warning === null ? "" : `; ${presentation.warning.statusLabel}`;
  const selectedWarningTitle =
    presentation.warning === null ? "" : ` · ${presentation.warning.statusLabel}`;
  const accessibleLabel = `${
    headline === null
      ? `Combined monitored resources unavailable; ${terminalCountLabel} terminals`
      : `Combined monitored resources: ${headline.memoryLabel} memory, ${headline.cpuLabel} CPU, ${headline.processCountLabel} processes; ${terminalCountLabel} terminals`
  }${selectedWarningLabel}`;
  const title = `${
    headline === null
      ? "Combined monitored resources unavailable"
      : `Combined monitored resources: ${headline.memoryLabel} memory · ${headline.cpuLabel} CPU · ${headline.processCountLabel} processes`
  }${selectedWarningTitle}`;

  return (
    <Popover onOpenChange={handleOpenChange}>
      <PopoverTrigger
        className="inline-flex h-5 items-center gap-1.5 rounded px-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
        aria-label={accessibleLabel}
        title={title}
      >
        <MemoryStickIcon className="size-3" />
        {!iconOnly ? (
          <>
            <span className="font-mono tabular-nums">{compactMemoryLabel}</span>
            <span className="text-muted-foreground">·</span>
            <CpuIcon className="size-3" />
            <span className="font-mono tabular-nums">{compactCpuLabel}</span>
            <span className="text-muted-foreground">·</span>
            <TerminalIcon className="size-3" />
          </>
        ) : null}
        <span className="font-mono tabular-nums">{terminalCountLabel}</span>
        {presentation.warning === null ? null : (
          <TriangleAlertIcon className="size-3 text-warning" aria-hidden="true" />
        )}
      </PopoverTrigger>
      <PopoverContent side="top" align="end" sideOffset={6}>
        <div className="w-96 space-y-3 text-xs">
          <div className="flex items-center gap-1.5 font-medium">
            <MemoryStickIcon className="size-3 text-muted-foreground" />
            <span>Resource Manager</span>
          </div>
          <p className="text-muted-foreground">
            Combined monitored resources for the selected host, separated by ownership.
          </p>

          {presentation.warning === null ? null : (
            <div
              className="flex items-start gap-2 rounded-md border border-warning/30 bg-warning/5 p-2 text-warning-foreground"
              role="status"
            >
              <TriangleAlertIcon className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
              <span>{presentation.warning.message}</span>
            </div>
          )}

          <div className="rounded-md border border-border/70 p-2.5">
            <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
              Combined
            </div>
            {headline === null ? (
              <div className="mt-1 font-medium text-muted-foreground">Unavailable</div>
            ) : (
              <div className="mt-1 grid grid-cols-3 gap-2">
                <div className="font-mono tabular-nums">{headline.memoryLabel} memory</div>
                <div className="font-mono tabular-nums">{headline.cpuLabel} CPU</div>
                <div className="font-mono tabular-nums">{headline.processCountLabel} processes</div>
              </div>
            )}
          </div>

          <div className="grid grid-cols-2 gap-2">
            <ResourceTotalsCard title="BiBCode Core" totals={presentation.core} />
            <ResourceTotalsCard title="External Tooling" totals={presentation.external} />
          </div>

          <div className="space-y-1.5">
            <div className="flex items-center gap-1 text-muted-foreground">
              <CpuIcon className="size-3" />
              <span>Highest consumers</span>
            </div>
            {presentation.consumers.map((consumer) => (
              <div
                key={consumer.processKey}
                className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 rounded px-1 py-0.5"
              >
                <span className="rounded-sm bg-muted px-1 py-0.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  {consumer.scopeLabel}
                </span>
                <span className="min-w-0">
                  <span className="block truncate font-medium text-foreground">
                    {consumer.label}
                  </span>
                  <span
                    className="block truncate text-xs text-muted-foreground"
                    title={consumer.command}
                  >
                    {consumer.command}
                  </span>
                </span>
                <span className="shrink-0 text-right font-mono text-xs tabular-nums">
                  <span className="block">{consumer.memoryLabel}</span>
                  <span className="block text-muted-foreground">{consumer.cpuLabel} CPU</span>
                </span>
              </div>
            ))}
            {presentation.consumers.length === 0 ? (
              <div className="text-muted-foreground">
                {headline === null ? "Unavailable" : "No process samples yet."}
              </div>
            ) : null}
          </div>

          {presentation.localCore === null ? null : (
            <div
              className="border-t border-border pt-3"
              role="group"
              aria-label={`This device BiBCode Core${
                presentation.localCore.warning === null
                  ? ""
                  : `; ${presentation.localCore.warning.statusLabel}`
              }`}
            >
              <div className="flex items-center justify-between gap-3">
                <div>
                  <div className="font-medium text-foreground">This device</div>
                  <div className="text-xs text-muted-foreground">
                    BiBCode Core
                    {presentation.localCore.totals?.coverageLabel === null ||
                    presentation.localCore.totals?.coverageLabel === undefined
                      ? null
                      : ` · ${presentation.localCore.totals.coverageLabel}`}
                  </div>
                </div>
                {presentation.localCore.totals === null ? (
                  <div className="font-medium text-muted-foreground">Unavailable</div>
                ) : (
                  <div className="shrink-0 text-right font-mono text-xs tabular-nums">
                    <div>{presentation.localCore.totals.memoryLabel}</div>
                    <div className="text-muted-foreground">
                      {presentation.localCore.totals.cpuLabel} CPU ·{" "}
                      {presentation.localCore.totals.processCountLabel} processes
                    </div>
                  </div>
                )}
              </div>
              {presentation.localCore.warning === null ? null : (
                <div
                  className="mt-2 flex items-start gap-2 rounded-md border border-warning/30 bg-warning/5 p-2 text-warning-foreground"
                  role="status"
                >
                  <TriangleAlertIcon className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
                  <span>{presentation.localCore.warning.message}</span>
                </div>
              )}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
