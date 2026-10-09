import type { ProfilerFinding, ProfilerStatus } from "@bibcode/contracts";
import type { UnifiedSettings } from "@bibcode/contracts/settings";
import { useAtomValue } from "@effect/atom-react";
import {
  CircleAlertIcon,
  DownloadIcon,
  FolderOpenIcon,
  InfoIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";

import { isDesktopHost } from "~/env";
import { useDiagnosticLogsDownload } from "../../diagnostics/useDiagnosticLogsDownload";
import { usePrimarySettings, useUpdatePrimarySettings } from "../../hooks/useSettings";
import { usePrimaryEnvironment } from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { primaryServerObservabilityAtom, serverEnvironment } from "../../state/server";
import { formatRelativeTimeLabel } from "../../timestampFormat";
import { useDocumentVisible } from "../pullRequests/useDocumentVisible";
import { Button } from "../ui/button";
import { Checkbox } from "../ui/checkbox";
import { RetryButton } from "../ui/retry-button";
import { Switch } from "../ui/switch";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { SettingsRow, SettingsSection } from "./settingsLayout";
import { useOpenDirectoryInEditor } from "./useOpenDirectoryInEditor";

const STATUS_REFRESH_MS = 5_000;
const MAX_FINDINGS = 20;
const PROFILER_FOLDER_NAME = "profiler";
const LOGS_FOLDER_NAME = "logs";
const NUMBER_FORMAT = new Intl.NumberFormat();

const selectEnableProfiler = (settings: UnifiedSettings) => settings.enableProfiler;

/**
 * The profiler writes to `<state dir>/profiler`, next to the `<state dir>/logs` folder the
 * server reports. Anything else is not a layout this knows, so it opens nothing.
 */
function profilerDirectoryFromLogsDirectory(logsDirectoryPath: string | null): string | null {
  if (logsDirectoryPath === null) return null;
  const trimmed = logsDirectoryPath.replace(/[\\/]+$/, "");
  const separatorIndex = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (trimmed.slice(separatorIndex + 1) !== LOGS_FOLDER_NAME) return null;
  return `${trimmed.slice(0, separatorIndex + 1)}${PROFILER_FOLDER_NAME}`;
}

function formatMegabytes(bytes: number): string {
  const megabytes = bytes / (1024 * 1024);
  if (megabytes >= 10) return String(Math.round(megabytes));
  if (bytes > 0 && megabytes < 0.1) return "<0.1";
  return String(Math.round(megabytes * 10) / 10);
}

function recordingLine(status: ProfilerStatus): string {
  const since =
    status.recordingSince === null
      ? ""
      : ` since ${formatRelativeTimeLabel(status.recordingSince)}`;
  const parts = [
    `Recording${since}`,
    `${formatMegabytes(status.diskBytes)} MB`,
    `${NUMBER_FORMAT.format(status.eventCount)} events`,
  ];
  if (status.droppedEvents > 0) parts.push(`${NUMBER_FORMAT.format(status.droppedEvents)} dropped`);
  return parts.join(" · ");
}

function newestFindings(findings: ReadonlyArray<ProfilerFinding>): ReadonlyArray<ProfilerFinding> {
  return findings.toSorted((a, b) => b.lastSeen.localeCompare(a.lastSeen)).slice(0, MAX_FINDINGS);
}

const SEVERITY_ICONS = {
  info: {
    icon: <InfoIcon aria-hidden className="size-3.5 text-muted-foreground" />,
    label: "Info",
  },
  warn: {
    icon: <TriangleAlertIcon aria-hidden className="size-3.5 text-warning" />,
    label: "Warning",
  },
  error: {
    icon: <CircleAlertIcon aria-hidden className="size-3.5 text-destructive" />,
    label: "Error",
  },
} as const;

function FindingRow({ finding }: { readonly finding: ProfilerFinding }) {
  const severity = SEVERITY_ICONS[finding.severity];
  return (
    <li className="flex min-w-0 items-center gap-2.5 border-t border-border/60 px-4 py-2.5 first:border-t-0 sm:px-5">
      <span className="shrink-0">{severity.icon}</span>
      <span className="sr-only">{severity.label}: </span>
      <span className="min-w-0 flex-1 truncate text-xs text-foreground" title={finding.title}>
        {finding.title}
      </span>
      <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-xs tabular-nums text-foreground/80">
        ×{finding.count}
      </span>
      <span className="shrink-0 text-xs text-muted-foreground">
        {formatRelativeTimeLabel(finding.lastSeen)}
      </span>
    </li>
  );
}

function FindingsList({
  status,
  hasError,
}: {
  readonly status: ProfilerStatus | null;
  readonly hasError: boolean;
}) {
  if (status === null && hasError) return null;
  return (
    <div className="border-t border-border/60">
      <h3 className="px-4 pt-3.5 pb-2 text-[13px] font-semibold tracking-[-0.01em] text-foreground sm:px-5">
        Findings
      </h3>
      {status === null ? (
        <p className="px-4 pb-3.5 text-xs text-muted-foreground sm:px-5">Loading findings...</p>
      ) : status.findings.length === 0 ? (
        <p className="px-4 pb-3.5 text-xs text-muted-foreground sm:px-5">No findings yet.</p>
      ) : (
        <ul aria-label="Findings" className="border-t border-border/60">
          {newestFindings(status.findings).map((finding) => (
            <FindingRow key={finding.id} finding={finding} />
          ))}
        </ul>
      )}
    </div>
  );
}

function OpenFolderButton({
  disabledReason,
  isOpening,
  onOpen,
}: {
  readonly disabledReason: ReactNode | null;
  readonly isOpening: boolean;
  readonly onOpen: () => void;
}) {
  const reasonId = useId();
  const button = (
    <Button
      size="xs"
      variant="outline"
      disabled={disabledReason !== null || isOpening}
      aria-describedby={disabledReason === null ? undefined : reasonId}
      onClick={onOpen}
    >
      <FolderOpenIcon className="size-3.5" />
      Open folder
    </Button>
  );
  if (disabledReason === null) return button;
  // A disabled button gets no pointer events, so the focusable wrapper carries the tooltip.
  return (
    <>
      <Tooltip>
        <TooltipTrigger render={<span className="inline-flex" tabIndex={0} />}>
          {button}
        </TooltipTrigger>
        <TooltipPopup side="top" className="max-w-[min(320px,calc(100vw-2rem))] text-wrap">
          {disabledReason}
        </TooltipPopup>
      </Tooltip>
      <span id={reasonId} className="sr-only">
        {disabledReason}
      </span>
    </>
  );
}

export function ProfilerSection() {
  const environment = usePrimaryEnvironment();
  const environmentId = environment?.environmentId ?? null;
  const enabled = usePrimarySettings(selectEnableProfiler);
  const updateSettings = useUpdatePrimarySettings();
  const observability = useAtomValue(primaryServerObservabilityAtom);
  const documentVisible = useDocumentVisible();
  const {
    data: status,
    error,
    isPending,
    refresh,
    revalidate,
  } = useEnvironmentQuery(
    environmentId === null ? null : serverEnvironment.profilerStatus({ environmentId, input: {} }),
  );
  const {
    isOpening,
    error: openFolderError,
    open: openDirectory,
  } = useOpenDirectoryInEditor("Unable to open the profiler folder.");
  const { isDownloading, download } = useDiagnosticLogsDownload();
  const [includeCrashDumps, setIncludeCrashDumps] = useState(false);

  // A refresh interrupts the read it replaces, so a slow connection would never finish one.
  const statusReadInFlight = useRef(false);
  useEffect(() => {
    statusReadInFlight.current = isPending;
  }, [isPending]);
  useEffect(() => {
    if (!enabled || !documentVisible) return;
    const timer = window.setInterval(() => {
      if (!statusReadInFlight.current) revalidate();
    }, STATUS_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [enabled, documentVisible, revalidate]);

  const profilerDirectory = profilerDirectoryFromLogsDirectory(
    observability?.logsDirectoryPath ?? null,
  );
  const environmentLabel = environment?.label.trim() || "the server";
  const openFolderDisabledReason: ReactNode | null = !isDesktopHost ? (
    <>
      The profiler folder is on {environmentLabel}. Use Export bundle or{" "}
      <span className="font-mono">bibcode profiler</span> on that machine.
    </>
  ) : profilerDirectory === null ? (
    "The profiler folder could not be located."
  ) : null;

  const handleToggle = (checked: boolean) => {
    // The server applies the setting live, so the status is read again once it is saved.
    void updateSettings({ enableProfiler: checked }).then(() => revalidate());
  };
  const handleOpenFolder = () => {
    if (profilerDirectory !== null) openDirectory(profilerDirectory);
  };

  const statusLine = !enabled
    ? "Off. Turn on to start recording. Crash checks for the previous run still happen."
    : status === null
      ? "Checking..."
      : status.enabled
        ? recordingLine(status)
        : "Starting...";

  return (
    <SettingsSection title="Profiler">
      <SettingsRow
        title="Record profiling data"
        description="Records crashes, memory and CPU trends, slow requests and worktree cleanup problems for this environment. Data stays on this machine. Uses about 1% CPU."
        status={
          <div className="space-y-1">
            <div>{statusLine}</div>
            {enabled && status?.capabilities.storage === "failing" ? (
              <div className="flex items-start gap-1.5 text-destructive">
                <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                <span>
                  Could not write profiling data. Check free disk space and folder permissions.
                </span>
              </div>
            ) : null}
            {error === null ? null : (
              <div className="flex flex-wrap items-center gap-2">
                <div className="flex items-start gap-1.5 text-destructive">
                  <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                  <span>{error}</span>
                </div>
                <RetryButton retrying={isPending} onRetry={refresh} />
              </div>
            )}
          </div>
        }
        control={
          <Switch
            checked={enabled}
            onCheckedChange={handleToggle}
            aria-label="Record profiling data"
          />
        }
      />
      <SettingsRow
        title="Recorded data"
        description="Open the profiler folder, or export it with the logs as a ZIP."
        status={
          openFolderError === null ? null : (
            <div className="flex items-start gap-1.5 text-destructive">
              <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
              <span>{openFolderError}</span>
            </div>
          )
        }
        control={
          <>
            <OpenFolderButton
              disabledReason={openFolderDisabledReason}
              isOpening={isOpening}
              onOpen={handleOpenFolder}
            />
            <Button
              size="xs"
              variant="outline"
              disabled={isDownloading}
              onClick={() => download({ includeCrashDumps })}
            >
              <DownloadIcon className="size-3.5" />
              {isDownloading ? "Exporting..." : "Export bundle"}
            </Button>
          </>
        }
      >
        <label className="flex items-start gap-2.5 pt-2.5 pb-3.5">
          <Checkbox
            checked={includeCrashDumps}
            onCheckedChange={(checked) => setIncludeCrashDumps(checked === true)}
            className="mt-0.5"
          />
          <span className="min-w-0 space-y-0.5">
            <span className="block text-[13px] font-medium text-foreground">
              Include crash dumps
            </span>
            <span className="block text-xs text-muted-foreground">
              Crash dumps can contain fragments of memory, including prompts or tokens. Only share
              them with people you trust.
            </span>
          </span>
        </label>
      </SettingsRow>
      <FindingsList status={status} hasError={error !== null} />
    </SettingsSection>
  );
}
