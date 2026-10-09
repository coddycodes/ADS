import { EnvironmentId, type ProfilerFinding, type ProfilerStatus } from "@bibcode/contracts";
import type { EnvironmentConnectionPhase } from "@bibcode/client-runtime/connection";
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
import { useEnvironmentSettings, useUpdateEnvironmentSettings } from "../../hooks/useSettings";
import {
  type EnvironmentPresentation,
  useEnvironments,
  usePrimaryEnvironment,
} from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { primaryServerObservabilityAtom, serverEnvironment } from "../../state/server";
import { formatRelativeTimeLabel } from "../../timestampFormat";
import { useDocumentVisible } from "../pullRequests/useDocumentVisible";
import { Button } from "../ui/button";
import { Checkbox } from "../ui/checkbox";
import { RetryButton } from "../ui/retry-button";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { SettingsRow, SettingsSection } from "./settingsLayout";
import { useOpenDirectoryInEditor } from "./useOpenDirectoryInEditor";

const STATUS_REFRESH_MS = 5_000;
const MAX_FINDINGS = 20;
const PROFILER_FOLDER_NAME = "profiler";
const LOGS_FOLDER_NAME = "logs";
const NUMBER_FORMAT = new Intl.NumberFormat();

// A selected environment in one of these phases is kept: it is connected, or on its way.
const STILL_CONNECTING_PHASES: ReadonlySet<EnvironmentConnectionPhase> = new Set([
  "connected",
  "available",
  "connecting",
  "reconnecting",
]);

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

/**
 * An action that can be unavailable. Its reason is shown in a tooltip and read out, because
 * a disabled button gets no pointer events: the focusable wrapper carries the tooltip.
 */
function ReasonedButton({
  icon,
  label,
  disabledReason,
  busy = false,
  onClick,
}: {
  readonly icon: ReactNode;
  readonly label: string;
  readonly disabledReason: ReactNode | null;
  readonly busy?: boolean;
  readonly onClick: () => void;
}) {
  const reasonId = useId();
  const button = (
    <Button
      size="xs"
      variant="outline"
      disabled={disabledReason !== null || busy}
      aria-describedby={disabledReason === null ? undefined : reasonId}
      onClick={onClick}
    >
      {icon}
      {label}
    </Button>
  );
  if (disabledReason === null) return button;
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

export function ProfilerSection({
  selectedEnvironmentId,
  onSelectEnvironment,
}: {
  /** The chosen environment, kept by the caller (the route) so a link can set it. */
  readonly selectedEnvironmentId: EnvironmentId | null;
  readonly onSelectEnvironment: (environmentId: EnvironmentId | null) => void;
}) {
  const { environments, isReady } = useEnvironments();
  const primaryEnvironment = usePrimaryEnvironment();
  const connected = environments.filter((candidate) => candidate.connection.phase === "connected");
  const primaryConnected =
    connected.find((candidate) => candidate.environmentId === primaryEnvironment?.environmentId) ??
    null;
  const environment =
    connected.find((candidate) => candidate.environmentId === selectedEnvironmentId) ??
    primaryConnected ??
    connected[0] ??
    primaryEnvironment ??
    null;
  // A choice whose environment has gone is dropped, so coming back online does not revive it.
  // One that has not connected yet (a reload on a link) is kept, as is any choice while the
  // catalog is loading, because the environment may simply not be known yet.
  const selectionGone =
    isReady &&
    selectedEnvironmentId !== null &&
    !environments.some(
      (candidate) =>
        candidate.environmentId === selectedEnvironmentId &&
        STILL_CONNECTING_PHASES.has(candidate.connection.phase),
    );
  useEffect(() => {
    if (selectionGone) onSelectEnvironment(null);
  }, [selectionGone, onSelectEnvironment]);
  // The export belongs to the primary environment and outlives a switch away from it.
  const exportBundle = useDiagnosticLogsDownload();
  if (environment === null) return null;
  // The requested environment is on its way, so another one shows in the meantime; say so.
  const waitingFor =
    selectedEnvironmentId !== null && selectedEnvironmentId !== environment.environmentId
      ? environments.find((candidate) => candidate.environmentId === selectedEnvironmentId)
      : undefined;

  return (
    <SettingsSection title="Profiler">
      {connected.length > 1 ||
      waitingFor !== undefined ||
      environment.environmentId !== primaryEnvironment?.environmentId ? (
        <SettingsRow
          title="Environment"
          description="Choose which environment's profiler to view and control."
          status={
            waitingFor === undefined
              ? null
              : `${waitingFor.label} is not connected. Showing ${environment.label}.`
          }
          control={
            connected.length > 1 ? (
              <Select
                value={environment.environmentId}
                onValueChange={(value) => {
                  if (value !== null) onSelectEnvironment(EnvironmentId.make(value));
                }}
              >
                <SelectTrigger aria-label="Environment" className="w-full sm:w-48">
                  <SelectValue>{environment.label}</SelectValue>
                </SelectTrigger>
                <SelectPopup align="end" alignItemWithTrigger={false}>
                  {environments.map((candidate) => (
                    <SelectItem
                      hideIndicator
                      disabled={candidate.connection.phase !== "connected"}
                      key={candidate.environmentId}
                      value={candidate.environmentId}
                    >
                      {candidate.label}
                    </SelectItem>
                  ))}
                </SelectPopup>
              </Select>
            ) : (
              // The only environment that can be controlled, and not the one the rest of
              // this page describes: name it instead of offering a choice.
              <span className="text-xs font-medium text-foreground">{environment.label}</span>
            )
          }
        />
      ) : null}
      <EnvironmentProfiler
        key={environment.environmentId}
        environment={environment}
        primaryEnvironment={primaryEnvironment}
        exportBundle={exportBundle}
      />
    </SettingsSection>
  );
}

function EnvironmentProfiler({
  environment,
  primaryEnvironment,
  exportBundle,
}: {
  readonly environment: EnvironmentPresentation;
  readonly primaryEnvironment: EnvironmentPresentation | null;
  readonly exportBundle: ReturnType<typeof useDiagnosticLogsDownload>;
}) {
  const environmentId = environment.environmentId;
  const isPrimary = environmentId === primaryEnvironment?.environmentId;
  const enabled = useEnvironmentSettings(environmentId, selectEnableProfiler);
  const updateSettings = useUpdateEnvironmentSettings(environmentId);
  const observability = useAtomValue(primaryServerObservabilityAtom);
  const documentVisible = useDocumentVisible();
  const {
    data: status,
    error,
    isPending,
    refresh,
    revalidate,
  } = useEnvironmentQuery(serverEnvironment.profilerStatus({ environmentId, input: {} }));
  const {
    isOpening,
    error: openFolderError,
    open: openDirectory,
  } = useOpenDirectoryInEditor("Unable to open the profiler folder.");
  const { isDownloading, download } = exportBundle;
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
  const environmentLabel = environment.label.trim() || "the server";
  const primaryLabel = primaryEnvironment?.label.trim() || "the main server";
  const profilerCommand = <span className="font-mono">bibcode profiler</span>;
  // The folder opens through this machine's editor and the bundle comes over the primary
  // connection, so both belong to the primary environment alone.
  const openFolderDisabledReason: ReactNode | null =
    !isDesktopHost || !isPrimary ? (
      <>
        The profiler folder is on {environmentLabel}. Use{" "}
        {isPrimary ? <>Export bundle or {profilerCommand}</> : profilerCommand} on that machine.
      </>
    ) : profilerDirectory === null ? (
      "The profiler folder could not be located."
    ) : status?.diskBytes === 0 ? (
      "Nothing recorded yet."
    ) : null;
  const exportDisabledReason: ReactNode | null = isPrimary ? null : (
    <>
      Export bundle covers {primaryLabel} only. Use {profilerCommand} on {environmentLabel}.
    </>
  );

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
    <>
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
            <ReasonedButton
              icon={<FolderOpenIcon className="size-3.5" />}
              label="Open folder"
              disabledReason={openFolderDisabledReason}
              busy={isOpening}
              onClick={handleOpenFolder}
            />
            <ReasonedButton
              icon={<DownloadIcon className="size-3.5" />}
              label={isDownloading ? "Exporting..." : "Export bundle"}
              disabledReason={exportDisabledReason}
              busy={isDownloading}
              onClick={() => download({ includeCrashDumps })}
            />
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
    </>
  );
}
