import {
  ChevronsLeftRightEllipsisIcon,
  EllipsisIcon,
  LoaderCircleIcon,
  PlusIcon,
  QrCodeIcon,
  RefreshCwIcon,
  TerminalIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { useAtomValue } from "@effect/atom-react";
import {
  type ReactNode,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type {
  DesktopDiscoveredSshHost,
  DesktopSshEnvironmentTarget,
  EnvironmentId,
} from "@bibcode/contracts";
import {
  connectionStatusText,
  type CompatVerdict,
  type PairingAddFailureReason,
  RelayConnectionRegistration,
  RelayConnectionTarget,
} from "@bibcode/client-runtime/connection";
import { classifyPairingEndpoint } from "@bibcode/shared/advertisedEndpoint";
import { parsePairingCode } from "@bibcode/shared/pairingCode";
import { findErrorTraceId } from "@bibcode/client-runtime/errors";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import { fanOutRemoteUpdateChecks } from "@bibcode/client-runtime/state/remoteUpdates";
import type { RelayClientEnvironmentRecord } from "@bibcode/contracts/relay";
import * as Option from "effect/Option";

import { useCopyToClipboard } from "../../../hooks/useCopyToClipboard";
import { cn } from "../../../lib/utils";
import { resolveServerConfigVersionMismatch } from "~/versionSkew";
import { hasCloudPublicConfig } from "~/cloud/publicConfig";
import { isDesktopLocalConnectionTarget } from "~/connection/desktopLocal";
import { selectRemoteUpdateControlCapability } from "~/connection/environmentCompat";
import { environmentCatalog } from "~/connection/catalog";
import {
  connectPairing as connectPairingAtom,
  connectRemoteServer as connectRemoteServerAtom,
  connectSshEnvironment as connectSshEnvironmentAtom,
} from "~/connection/onboarding";
import { desktopSshHostsStateAtom } from "~/state/desktopSshHosts";
import {
  remoteUpdateEnvironment,
  useRemoteUpdateCheckState,
  useRemoteUpdateRun,
  useRequestRemoteUpdateConfirmation,
  useDismissRemoteUpdate,
} from "~/state/remoteUpdates";
import {
  isRemoteUpdateRunActive,
  type RemoteUpdateRunState,
} from "@bibcode/client-runtime/state/remoteUpdateCoordinator";
import {
  manualUpdateSteps,
  remoteUpdateActionLabel,
  remoteUpdateFailureMessage,
  visibleRemoteUpdateRun,
} from "../remoteUpdatePresentation";
import {
  type EnvironmentPresentation,
  useEnvironments,
  usePrimaryEnvironment,
  useRelayEnvironmentDiscovery,
} from "~/state/environments";
import { relayEnvironmentDiscovery } from "~/state/relay";
import { useEnvironmentQuery } from "~/state/query";
import { useAtomCommand } from "../../../state/use-atom-command";
import { environmentSession } from "~/state/session";
import { useThreadShells } from "~/state/entities";
import { AnimatedHeight } from "../../AnimatedHeight";
import { Button } from "../../ui/button";
import { Badge } from "../../ui/badge";
import { Checkbox } from "../../ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../ui/collapsible";
import {
  Dialog,
  DialogDescription,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
  DialogTrigger,
} from "../../ui/dialog";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../../ui/alert-dialog";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../../ui/empty";
import { Input } from "../../ui/input";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger } from "../../ui/menu";
import { ScrollArea } from "../../ui/scroll-area";
import { Skeleton } from "../../ui/skeleton";
import { Textarea } from "../../ui/textarea";
import { stackedThreadToast, toastManager } from "../../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../../ui/tooltip";
import { SettingsSection } from "../settingsLayout";
import {
  ServerUpdateBadge,
  type ServerUpdateStatus,
  serverUpdateBadgeVariant,
  serverUpdateStatusFromQuery,
} from "../ServerUpdateBadge";
import {
  ConnectionStatusDot,
  EMPTY_DISCOVERED_SSH_HOSTS,
  formatDesktopSshConnectionError,
  formatDesktopSshTarget,
  ITEM_ROW_CLASSNAME,
  ITEM_ROW_INNER_CLASSNAME,
  parseManualDesktopSshTarget,
  parseRemotePairingFields,
  tryResolveRemotePairingHostInput,
} from "./shared";
import { RenameServerDialog, type RenameServerRequest } from "./RenameServerDialog";
import {
  ADD_SERVER_FAILURE_REASONS,
  countRunningThreadsForEnvironment,
  describeAddServerFailure,
  describeCompatBadge,
  describeRenameServerFailure,
  describeSshEnvironmentAddedToast,
  formatServerVersionLabel,
  isLoopbackAcknowledgementRequired,
  normalizePairingCodeInput,
  resolvePairingAddFailureDetail,
  resolvePairingAddFailureReason,
  resolveTransportBadge,
} from "./connectPresentation";

type RemoteServerRowProps = {
  environment: EnvironmentPresentation;
  compat: CompatVerdict | null;
  remoteUpdateControl: boolean;
  updateStatus: ServerUpdateStatus;
  /** A check, or the first status read, is running: Check reads "Checking…" and waits. */
  checkInFlight: boolean;
  run: RemoteUpdateRunState | null;
  removingEnvironmentId: EnvironmentId | null;
  onConnect: (environmentId: EnvironmentId) => void;
  onDisconnect: (environmentId: EnvironmentId) => void;
  onRequestRemove: (environmentId: EnvironmentId, label: string) => void;
  onRequestRename: (request: RenameServerRequest) => void;
  onCheckForUpdate: () => void;
  onRequestUpdate: () => void;
  onDismissUpdate: () => void;
  /** Re-reads a failed update status. */
  onRetryUpdate: () => void;
};

function RemoteServerRow({
  environment,
  compat,
  remoteUpdateControl,
  updateStatus,
  checkInFlight,
  run,
  removingEnvironmentId,
  onConnect,
  onDisconnect,
  onRequestRemove,
  onRequestRename,
  onCheckForUpdate,
  onRequestUpdate,
  onDismissUpdate,
  onRetryUpdate,
}: RemoteServerRowProps) {
  const environmentId = environment.environmentId;
  const connectionState = environment.connection.phase;
  const isConnected = connectionState === "connected";
  const isDisconnected = connectionState === "available" || connectionState === "offline";
  const isConnecting = connectionState === "connecting" || connectionState === "reconnecting";
  const stateDotClassName =
    connectionState === "connected"
      ? "bg-success"
      : connectionState === "connecting" || connectionState === "reconnecting"
        ? "bg-warning"
        : connectionState === "error"
          ? "bg-destructive"
          : "bg-muted-foreground/40";
  const statusTooltip = connectionStatusText(environment.connection);
  const statusLine = environment.connection.error
    ? "error"
    : environment.connection.notice
      ? "notice"
      : null;
  const errorTraceId = environment.connection.traceId;
  const { copyToClipboard: copyTraceIdToClipboard } = useCopyToClipboard<{ traceId: string }>({
    target: "trace ID",
    onCopy: ({ traceId }) => {
      toastManager.add({
        type: "success",
        title: "Trace ID copied",
        description: traceId,
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not copy trace ID",
          description: error.message,
        }),
      );
    },
  });
  const copyTraceId = useCallback(
    (traceId: string) => {
      copyTraceIdToClipboard(traceId, { traceId });
    },
    [copyTraceIdToClipboard],
  );
  const { copyToClipboard: copyUpdateInstructionsToClipboard } = useCopyToClipboard<{
    instructions: string;
  }>({
    target: "manual update instructions",
    onCopy: () => {
      toastManager.add({
        type: "success",
        title: "Update instructions copied",
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not copy update instructions",
          description: error.message,
        }),
      );
    },
  });
  const versionMismatch = resolveServerConfigVersionMismatch(environment.serverConfig);
  const versionLabel = formatServerVersionLabel(
    environment.serverConfig?.environment?.serverVersion,
  );
  const compatBadge = describeCompatBadge(compat);
  const transportBadge = resolveTransportBadge(environment);
  const updateSnapshot = updateStatus.snapshot;
  const runActive = isRemoteUpdateRunActive(run);
  const failedRun = run?.phase === "failed" ? run : null;
  const updateVariant = serverUpdateBadgeVariant(updateStatus);
  const updateInstructions =
    updateSnapshot !== null &&
    (updateSnapshot.support.installMode === "manual" ||
      failedRun?.failure.kind === "manual-required")
      ? manualUpdateSteps({
          installKind: updateSnapshot.support.installKind,
          os: environment.serverConfig?.environment.platform?.os ?? "unknown",
          arch: environment.serverConfig?.environment.platform?.arch ?? "other",
          sshLaunched: environment.entry.target._tag === "SshConnectionTarget",
          serverVersion: updateSnapshot.serverVersion,
        })
      : null;
  const statusUnavailable =
    versionLabel === null && compatBadge === null && environment.connection.error !== null;
  const sshTarget =
    environment.entry.target._tag === "SshConnectionTarget" &&
    Option.isSome(environment.entry.profile) &&
    environment.entry.profile.value._tag === "SshConnectionProfile"
      ? environment.entry.profile.value.target
      : null;
  const metadataBits = [
    sshTarget ? `SSH ${formatDesktopSshTarget(sshTarget)}` : null,
    environment.relayManaged ? "BiBCode Connect" : null,
  ].filter((value): value is string => value !== null);

  // The WSL backend is a desktop-managed local backend (it surfaces as a bearer
  // environment whose connection id is prefixed "local:"), not a remote
  // environment you connect to or remove here — its lifecycle is driven by the
  // WSL on/off + distro picker on this page.
  const isWslEnvironment = isDesktopLocalConnectionTarget(environment.entry.target);

  return (
    <div className={ITEM_ROW_CLASSNAME}>
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex min-h-5 items-center gap-1.5">
            <ConnectionStatusDot
              tooltipText={statusTooltip}
              dotClassName={stateDotClassName}
              pingClassName={
                connectionState === "connecting" || connectionState === "reconnecting"
                  ? "bg-warning/60 duration-2000"
                  : null
              }
            />
            <h3 className="text-sm font-medium text-foreground">{environment.label}</h3>
            {isDisconnected ? (
              <span className="text-xs text-muted-foreground">Disconnected</span>
            ) : null}
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            {versionLabel ? (
              <span className="text-xs text-muted-foreground">{versionLabel}</span>
            ) : null}
            {statusUnavailable ? (
              <span className="text-xs text-muted-foreground">Status unavailable</span>
            ) : null}
            {compatBadge ? (
              <Badge variant={compatBadge.tone === "destructive" ? "destructive" : "warning"}>
                {compatBadge.label}
              </Badge>
            ) : null}
            {transportBadge ? (
              transportBadge.kind === "unencrypted" ? (
                <Tooltip>
                  <TooltipTrigger
                    render={<Badge variant="warning">{transportBadge.label}</Badge>}
                  />
                  <TooltipPopup side="top">{transportBadge.guidance}</TooltipPopup>
                </Tooltip>
              ) : (
                <Badge variant="outline">{transportBadge.label}</Badge>
              )
            ) : null}
            {remoteUpdateControl ? (
              <ServerUpdateBadge
                {...updateStatus}
                run={run}
                name={environment.label}
                onRetry={onRetryUpdate}
              />
            ) : null}
            {metadataBits.length > 0 ? (
              <span className="text-xs text-muted-foreground">{metadataBits.join(" · ")}</span>
            ) : null}
          </div>
          {versionMismatch ? (
            <p className="flex items-center gap-1 text-warning text-xs">
              <TriangleAlertIcon className="size-3.5 shrink-0" />
              Version drift: client {versionMismatch.clientVersion}, server{" "}
              {versionMismatch.serverVersion}.
            </p>
          ) : null}
          {statusLine === "error" ? (
            <p className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 text-destructive text-xs">
              {/* Wraps so the action half of the message stays readable; the
                  clamp only bounds an unusually long detail, which the status
                  dot's tooltip still shows in full. */}
              <span className="min-w-0 line-clamp-3 wrap-break-word">
                {connectionStatusText({
                  phase: connectionState,
                  error: environment.connection.error,
                  traceId: errorTraceId,
                })}
              </span>
              {errorTraceId ? (
                <button
                  type="button"
                  className="shrink-0 underline underline-offset-2"
                  onClick={() => copyTraceId(errorTraceId)}
                >
                  Copy trace ID
                </button>
              ) : null}
            </p>
          ) : statusLine === "notice" ? (
            <p
              role="status"
              aria-live="polite"
              className="flex min-w-0 flex-wrap items-start gap-x-2 gap-y-0.5 text-xs text-muted-foreground"
            >
              <LoaderCircleIcon
                aria-hidden="true"
                className="mt-px size-3.5 shrink-0 animate-spin"
              />
              <span className="min-w-0 flex-1 wrap-break-word">
                {environment.connection.notice}
              </span>
            </p>
          ) : null}
          {failedRun ? (
            <p role="alert" className="text-xs text-destructive">
              {remoteUpdateFailureMessage(environment.label, failedRun.failure)}
            </p>
          ) : null}
          {updateInstructions ? (
            <Collapsible>
              <CollapsibleTrigger className="text-xs text-muted-foreground underline underline-offset-2">
                Show update steps
              </CollapsibleTrigger>
              <CollapsibleContent className="mt-2 space-y-2">
                <div className="overflow-x-auto rounded-md">
                  <pre data-text-surface="card" className="bg-card p-2 text-xs whitespace-pre-wrap">
                    {updateInstructions}
                  </pre>
                </div>
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() =>
                    copyUpdateInstructionsToClipboard(updateInstructions, {
                      instructions: updateInstructions,
                    })
                  }
                >
                  Copy
                </Button>
              </CollapsibleContent>
            </Collapsible>
          ) : null}
        </div>
        <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto sm:justify-end">
          {remoteUpdateControl ? (
            <Button
              size="xs"
              variant="ghost"
              disabled={checkInFlight || runActive}
              onClick={onCheckForUpdate}
            >
              {checkInFlight
                ? "Checking…"
                : updateVariant === "check-failed" || updateVariant === "error"
                  ? "Check again"
                  : "Check"}
            </Button>
          ) : null}
          {remoteUpdateControl &&
          updateSnapshot?.support.installMode === "interactive" &&
          updateVariant === "update-available" &&
          run === null &&
          updateSnapshot.latestVersion !== null ? (
            <Button size="xs" disabled={checkInFlight} onClick={onRequestUpdate}>
              {remoteUpdateActionLabel(updateSnapshot.latestVersion)}
            </Button>
          ) : null}
          {failedRun ? (
            <>
              <Button size="xs" variant="outline" onClick={onRequestUpdate}>
                Retry
              </Button>
              <Button size="xs" variant="ghost" onClick={onDismissUpdate}>
                Dismiss
              </Button>
            </>
          ) : null}
          {isWslEnvironment ? (
            <Tooltip>
              <TooltipTrigger
                render={
                  <Button size="xs" variant="outline" disabled>
                    Managed above
                  </Button>
                }
              />
              <TooltipPopup side="top" className="max-w-80 whitespace-pre-wrap leading-tight">
                The WSL backend is managed by the WSL setting above — turn it on or off there.
              </TooltipPopup>
            </Tooltip>
          ) : (
            <>
              <Button
                size="xs"
                variant="outline"
                disabled={isConnecting || removingEnvironmentId === environmentId}
                onClick={() =>
                  void (isConnected ? onDisconnect(environmentId) : onConnect(environmentId))
                }
              >
                {isConnected ? "Disconnect" : isConnecting ? "Connecting…" : "Connect"}
              </Button>
              <Menu>
                <MenuTrigger
                  render={
                    <Button
                      size="icon-xs"
                      variant="ghost"
                      aria-label={`More actions for ${environment.label}`}
                      disabled={removingEnvironmentId === environmentId}
                    >
                      <EllipsisIcon aria-hidden />
                    </Button>
                  }
                />
                <MenuPopup align="end">
                  <MenuItem
                    onClick={() =>
                      onRequestRename({
                        environmentId,
                        label: environment.label,
                        serverLabel: environment.serverConfig?.environment.label ?? null,
                      })
                    }
                  >
                    Rename…
                  </MenuItem>
                  <MenuSeparator />
                  <MenuItem
                    variant="destructive"
                    onClick={() => onRequestRemove(environmentId, environment.label)}
                  >
                    Remove server…
                  </MenuItem>
                </MenuPopup>
              </Menu>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function RemoteServerRowFromSession(
  props: Omit<
    RemoteServerRowProps,
    | "compat"
    | "remoteUpdateControl"
    | "updateStatus"
    | "checkInFlight"
    | "run"
    | "onCheckForUpdate"
    | "onRequestUpdate"
    | "onDismissUpdate"
    | "onRetryUpdate"
  > & {
    readonly updateRefreshEpoch: number;
  },
) {
  const { updateRefreshEpoch, ...rowProps } = props;
  const compat = useAtomValue(
    environmentSession.compatVerdictAtom(props.environment.environmentId),
  );
  const environmentId = props.environment.environmentId;
  const remoteUpdateControl = selectRemoteUpdateControlCapability(props.environment.serverConfig);
  const run = useRemoteUpdateRun(remoteUpdateControl ? environmentId : null);
  const runActive = isRemoteUpdateRunActive(run);
  const requestUpdate = useRequestRemoteUpdateConfirmation();
  const dismissUpdate = useDismissRemoteUpdate();
  const updateQuery = useEnvironmentQuery(
    remoteUpdateControl && !runActive
      ? remoteUpdateEnvironment.snapshot({ environmentId, input: {} })
      : null,
  );
  const refreshUpdateStatus = updateQuery.refresh;
  const revalidateUpdateStatus = updateQuery.revalidate;
  const updateCheck = useRemoteUpdateCheckState(remoteUpdateControl ? environmentId : null);
  const runCheck = useAtomCommand(remoteUpdateEnvironment.check, { reportFailure: false });

  // A fan-out check's epoch is an automatic re-read; it keeps a transport cut-off latched.
  useEffect(() => {
    if (remoteUpdateControl && updateRefreshEpoch > 0) {
      revalidateUpdateStatus();
    }
  }, [revalidateUpdateStatus, remoteUpdateControl, updateRefreshEpoch]);

  // Re-reads after a check or install are automatic; only Retry clears a cut-off latch.
  const checkForUpdate = useCallback(async () => {
    // The check records its own progress and failure in the shared check state.
    await runCheck({ environmentId, input: {} });
    revalidateUpdateStatus();
  }, [environmentId, revalidateUpdateStatus, runCheck]);

  const queryStatus = serverUpdateStatusFromQuery(updateQuery, {
    connected: props.environment.connection.phase === "connected",
    check: updateCheck,
  });
  const updateStatus = queryStatus;
  return (
    <RemoteServerRow
      {...rowProps}
      compat={compat}
      remoteUpdateControl={remoteUpdateControl}
      updateStatus={updateStatus}
      // Background status re-reads keep the known snapshot, so they never flip Check.
      checkInFlight={serverUpdateBadgeVariant(updateStatus) === "checking"}
      run={visibleRemoteUpdateRun(run, updateStatus.snapshot)}
      onCheckForUpdate={() => void checkForUpdate()}
      onRequestUpdate={() =>
        requestUpdate({
          environmentId,
          name: props.environment.label,
          targetVersion: updateStatus.snapshot?.latestVersion ?? null,
          progress:
            props.environment.serverConfig?.environment.capabilities.remoteUpdateProgress ?? false,
        })
      }
      onDismissUpdate={() => dismissUpdate(environmentId)}
      onRetryUpdate={refreshUpdateStatus}
    />
  );
}

interface DesktopSshHostRowProps {
  target: DesktopDiscoveredSshHost;
  connectingHostAlias: string | null;
  onConnect: (target: DesktopDiscoveredSshHost) => void;
}

const DesktopSshHostRow = memo(function DesktopSshHostRow({
  target,
  connectingHostAlias,
  onConnect,
}: DesktopSshHostRowProps) {
  const address = formatDesktopSshTarget(target);
  const showAddress = address !== target.alias;
  const buttonLabel = connectingHostAlias === target.alias ? "Adding…" : "Add environment";

  return (
    <div className="border-t border-border/60 px-4 py-3 first:border-t-0 sm:px-5">
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-sm font-medium text-foreground">{target.alias}</h3>
          {showAddress ? <p className="truncate text-xs text-muted-foreground">{address}</p> : null}
        </div>
        <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto sm:justify-end">
          <Button
            size="xs"
            variant="outline"
            disabled={connectingHostAlias === target.alias}
            onClick={() => onConnect(target)}
          >
            {connectingHostAlias === target.alias ? (
              <RefreshCwIcon className="size-3 animate-spin" />
            ) : null}
            {buttonLabel}
          </Button>
        </div>
      </div>
    </div>
  );
});
function EmptyRemoteEnvironments({ cloudEnabled = true }: { readonly cloudEnabled?: boolean }) {
  return (
    <Empty className="min-h-52">
      <EmptyMedia variant="icon">
        <ChevronsLeftRightEllipsisIcon />
      </EmptyMedia>
      <EmptyHeader>
        <EmptyTitle>No saved remote environments</EmptyTitle>
        <EmptyDescription>
          {cloudEnabled
            ? "Click “Add environment” to pair another environment, or connect one from BiBCode Connect."
            : "Click “Add environment” to pair another environment."}
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function RemoteEnvironmentRowsSkeleton() {
  return (
    <div className={ITEM_ROW_CLASSNAME}>
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0 flex-1 space-y-2">
          <Skeleton className="h-4 w-32 rounded-full" />
          <Skeleton className="h-3 w-20 rounded-full" />
        </div>
        <Skeleton className="h-7 w-16 rounded-md" />
      </div>
    </div>
  );
}

function ConfiguredCloudRemoteEnvironmentRows({
  primaryEnvironmentId,
  savedEnvironmentIds,
}: {
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly savedEnvironmentIds: ReadonlyArray<EnvironmentId>;
}) {
  const environmentsState = useRelayEnvironmentDiscovery();
  const registerEnvironment = useAtomCommand(environmentCatalog.register, {
    reportFailure: false,
  });
  const refreshRelayEnvironments = useAtomCommand(relayEnvironmentDiscovery.refresh, {
    reportFailure: false,
  });
  const connectRelayEnvironment = useCallback(
    (environment: RelayClientEnvironmentRecord) =>
      registerEnvironment(
        new RelayConnectionRegistration({
          target: new RelayConnectionTarget({
            environmentId: environment.environmentId,
            label: environment.label,
          }),
        }),
      ),
    [registerEnvironment],
  );
  const [connectingEnvironmentId, setConnectingEnvironmentId] = useState<EnvironmentId | null>(
    null,
  );
  const savedIds = useMemo(() => new Set(savedEnvironmentIds), [savedEnvironmentIds]);

  useEffect(() => {
    void refreshRelayEnvironments();
  }, [refreshRelayEnvironments]);

  const connectEnvironment = async (environment: RelayClientEnvironmentRecord) => {
    setConnectingEnvironmentId(environment.environmentId);
    const result = await connectRelayEnvironment(environment);
    setConnectingEnvironmentId(null);
    if (result._tag === "Success") {
      toastManager.add({
        type: "success",
        title: "Environment connected",
        description: `${environment.label} is available through BiBCode Connect.`,
      });
      return;
    }
    if (isAtomCommandInterrupted(result)) {
      return;
    }
    const cause = squashAtomCommandFailure(result);
    const message =
      cause instanceof Error ? cause.message : "Could not connect the BiBCode Connect environment.";
    const traceId = findErrorTraceId(cause);
    console.error("[bibcode-connect] Could not connect environment", { message, traceId, cause });
    toastManager.add({
      type: "error",
      title: "Could not connect environment",
      description: message,
      data: traceId
        ? {
            secondaryActionProps: {
              children: "Copy trace ID",
              onClick: () => void navigator.clipboard?.writeText(traceId),
            },
          }
        : undefined,
    });
  };

  const connectableEnvironments = [...environmentsState.environments.values()].filter(
    ({ environment }) =>
      environment.environmentId !== primaryEnvironmentId &&
      !savedIds.has(environment.environmentId),
  );

  if (
    savedEnvironmentIds.length === 0 &&
    environmentsState.refreshing &&
    environmentsState.environments.size === 0
  ) {
    return <RemoteEnvironmentRowsSkeleton />;
  }

  if (savedEnvironmentIds.length === 0 && connectableEnvironments.length === 0) {
    return <EmptyRemoteEnvironments />;
  }

  return connectableEnvironments.map(({ environment, availability, error }) => (
    <div key={environment.environmentId} className={ITEM_ROW_CLASSNAME}>
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <ConnectionStatusDot
              dotClassName={
                availability === "online"
                  ? "bg-success"
                  : availability === "error"
                    ? "bg-destructive"
                    : availability === "checking"
                      ? "bg-warning"
                      : "bg-muted-foreground/35"
              }
              pingClassName={availability === "checking" ? "bg-warning/60 duration-2000" : null}
              tooltipText={
                availability === "online"
                  ? "Relay online"
                  : availability === "offline"
                    ? "Relay offline"
                    : availability === "checking"
                      ? "Checking relay status"
                      : (Option.getOrNull(error)?.message ?? "Relay status unavailable")
              }
            />
            <p className="truncate text-sm font-medium">{environment.label}</p>
          </div>
          <p
            className={cn(
              "mt-1 truncate text-xs",
              availability === "error" ? "text-destructive" : "text-muted-foreground",
            )}
          >
            {availability === "online"
              ? "Available · Relay online"
              : availability === "offline"
                ? "Available · Relay offline"
                : availability === "checking"
                  ? "Available · Checking relay status…"
                  : (Option.getOrNull(error)?.message ?? "Available · Relay status unavailable")}
          </p>
        </div>
        <Button
          size="sm"
          disabled={connectingEnvironmentId !== null}
          onClick={() => void connectEnvironment(environment)}
        >
          {connectingEnvironmentId === environment.environmentId ? "Connecting…" : "Connect"}
        </Button>
      </div>
    </div>
  ));
}

function CloudRemoteEnvironmentRows({
  primaryEnvironmentId,
  savedEnvironmentIds,
}: {
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly savedEnvironmentIds: ReadonlyArray<EnvironmentId>;
}) {
  return hasCloudPublicConfig() ? (
    <ConfiguredCloudRemoteEnvironmentRows
      primaryEnvironmentId={primaryEnvironmentId}
      savedEnvironmentIds={savedEnvironmentIds}
    />
  ) : savedEnvironmentIds.length === 0 ? (
    <EmptyRemoteEnvironments cloudEnabled={false} />
  ) : null;
}

export const SERVER_UPDATE_CHECK_ENABLED = true;

export function ConnectTab({
  initialPairingCode = null,
  initialAddServerOpen = false,
  onPairingCodeConsumed,
  onAddServerActionConsumed,
  showServerUpdateCheck = SERVER_UPDATE_CHECK_ENABLED,
}: {
  readonly initialPairingCode?: string | null;
  readonly initialAddServerOpen?: boolean;
  readonly onPairingCodeConsumed?: () => void;
  readonly onAddServerActionConsumed?: () => void;
  readonly showServerUpdateCheck?: boolean;
}) {
  const desktopBridge = window.desktopBridge;
  const { environments } = useEnvironments();
  const threadShells = useThreadShells();
  const primaryEnvironment = usePrimaryEnvironment();
  const connectPairing = useAtomCommand(connectPairingAtom, { reportFailure: false });
  const connectRemoteServer = useAtomCommand(connectRemoteServerAtom, { reportFailure: false });
  const connectSshEnvironment = useAtomCommand(connectSshEnvironmentAtom, {
    reportFailure: false,
  });
  const connectEnvironment = useAtomCommand(environmentCatalog.connect, { reportFailure: false });
  const disconnectEnvironment = useAtomCommand(environmentCatalog.disconnect, {
    reportFailure: false,
  });
  const removeEnvironment = useAtomCommand(environmentCatalog.remove, { reportFailure: false });
  const renameEnvironment = useAtomCommand(environmentCatalog.rename, { reportFailure: false });
  const runRemoteUpdateCheck = useAtomCommand(remoteUpdateEnvironment.check, {
    reportFailure: false,
  });
  const primaryEnvironmentId = primaryEnvironment?.environmentId ?? null;
  const savedEnvironments = useMemo(
    () =>
      environments
        .filter((environment) => environment.entry.target._tag !== "PrimaryConnectionTarget")
        .toSorted((left, right) => left.label.localeCompare(right.label)),
    [environments],
  );
  const savedEnvironmentIds = useMemo(
    () => savedEnvironments.map((environment) => environment.environmentId),
    [savedEnvironments],
  );
  const updateCapableEnvironmentIds = useMemo(
    () =>
      savedEnvironments
        .filter((environment) => selectRemoteUpdateControlCapability(environment.serverConfig))
        .map((environment) => environment.environmentId),
    [savedEnvironments],
  );
  const savedDesktopSshEnvironmentKeys = useMemo(() => {
    const keys = new Set<string>();
    for (const environment of savedEnvironments) {
      const profile = environment.entry.profile;
      if (
        environment.entry.target._tag !== "SshConnectionTarget" ||
        Option.isNone(profile) ||
        profile.value._tag !== "SshConnectionProfile"
      ) {
        continue;
      }
      const target = profile.value.target;
      keys.add(target.alias);
      keys.add(formatDesktopSshTarget(target));
    }
    return keys;
  }, [savedEnvironments]);
  const [sshConnectionError, setSshConnectionError] = useState<string | null>(null);
  const [connectingSshHostAlias, setConnectingSshHostAlias] = useState<string | null>(null);

  const [addBackendDialogOpen, setAddBackendDialogOpen] = useState(false);
  const initialPairingCodeConsumedRef = useRef(false);
  const initialAddServerActionConsumedRef = useRef(false);
  const [savedBackendMode, setSavedBackendMode] = useState<"pairing-code" | "manual" | "ssh">(
    "pairing-code",
  );
  const [pairingCodeInput, setPairingCodeInput] = useState("");
  const [serverAlias, setServerAlias] = useState("");
  const [tunnelAcknowledged, setTunnelAcknowledged] = useState(false);
  const [flowDemandsAcknowledgement, setFlowDemandsAcknowledgement] = useState(false);
  const [addServerFailure, setAddServerFailure] = useState<{
    readonly reason: PairingAddFailureReason;
    readonly detail: string | null;
  } | null>(null);
  const [savedBackendHost, setSavedBackendHost] = useState("");
  const [savedBackendPairingCode, setSavedBackendPairingCode] = useState("");
  const [savedBackendSshHost, setSavedBackendSshHost] = useState("");
  const [savedBackendSshUsername, setSavedBackendSshUsername] = useState("");
  const [savedBackendSshPort, setSavedBackendSshPort] = useState("");
  const [savedBackendError, setSavedBackendError] = useState<string | null>(null);
  const [isAddingSavedBackend, setIsAddingSavedBackend] = useState(false);
  const [removingSavedEnvironmentId, setRemovingSavedEnvironmentId] =
    useState<EnvironmentId | null>(null);
  const [removalCandidate, setRemovalCandidate] = useState<{
    readonly environmentId: EnvironmentId;
    readonly label: string;
  } | null>(null);
  const [renameRequest, setRenameRequest] = useState<RenameServerRequest | null>(null);
  const runningRemovalCount =
    removalCandidate === null
      ? 0
      : countRunningThreadsForEnvironment(threadShells, removalCandidate.environmentId);
  const [checkingAllServerUpdates, setCheckingAllServerUpdates] = useState(false);
  const [updateRefreshEpochs, setUpdateRefreshEpochs] = useState<
    ReadonlyMap<EnvironmentId, number>
  >(new Map());
  const onCheckForServerUpdates = useCallback(async () => {
    if (checkingAllServerUpdates || updateCapableEnvironmentIds.length === 0) return;
    setCheckingAllServerUpdates(true);
    await fanOutRemoteUpdateChecks(updateCapableEnvironmentIds, async (environmentId) => {
      const result = await runRemoteUpdateCheck({ environmentId, input: {} });
      setUpdateRefreshEpochs((current) => {
        const next = new Map(current);
        next.set(environmentId, (current.get(environmentId) ?? 0) + 1);
        return next;
      });
      return result;
    });
    setCheckingAllServerUpdates(false);
  }, [checkingAllServerUpdates, runRemoteUpdateCheck, updateCapableEnvironmentIds]);
  const consumeInitialPairingCode = useCallback(() => {
    if (initialPairingCode === null || initialPairingCodeConsumedRef.current) return;
    initialPairingCodeConsumedRef.current = true;
    onPairingCodeConsumed?.();
  }, [initialPairingCode, onPairingCodeConsumed]);
  const normalizedPairingCode = normalizePairingCodeInput(pairingCodeInput);
  const decodedPairingCode = useMemo(() => {
    if (normalizedPairingCode === null) return null;
    try {
      return parsePairingCode(normalizedPairingCode);
    } catch {
      return null;
    }
  }, [normalizedPairingCode]);
  const requiresTunnelAcknowledgement =
    flowDemandsAcknowledgement ||
    (decodedPairingCode !== null &&
      (decodedPairingCode.reach === "this-computer" ||
        classifyPairingEndpoint(decodedPairingCode.endpoint) === "loopback"));
  const [appliedInitialPairingCode, setAppliedInitialPairingCode] = useState<string | null>(null);
  if (initialPairingCode !== appliedInitialPairingCode) {
    setAppliedInitialPairingCode(initialPairingCode);
    if (initialPairingCode !== null) {
      setPairingCodeInput(initialPairingCode);
      setSavedBackendMode("pairing-code");
      setTunnelAcknowledged(false);
      setFlowDemandsAcknowledgement(false);
      setAddServerFailure(null);
      setSavedBackendError(null);
      setAddBackendDialogOpen(true);
    }
  }
  useLayoutEffect(() => {
    if (initialPairingCode !== null) initialPairingCodeConsumedRef.current = false;
  }, [initialPairingCode]);
  useEffect(() => {
    if (!initialAddServerOpen || initialAddServerActionConsumedRef.current) return;
    initialAddServerActionConsumedRef.current = true;
    setSavedBackendMode("pairing-code");
    setTunnelAcknowledged(false);
    setFlowDemandsAcknowledgement(false);
    setAddServerFailure(null);
    setSavedBackendError(null);
    setAddBackendDialogOpen(true);
    onAddServerActionConsumed?.();
  }, [initialAddServerOpen, onAddServerActionConsumed]);
  const desktopSshHosts = useEnvironmentQuery(
    desktopBridge && addBackendDialogOpen && savedBackendMode === "ssh"
      ? desktopSshHostsStateAtom
      : null,
  );
  const discoveredSshHosts = desktopSshHosts.data ?? EMPTY_DISCOVERED_SSH_HOSTS;
  const unsavedDiscoveredSshHosts = useMemo(
    () =>
      discoveredSshHosts.filter((target) => {
        const address = formatDesktopSshTarget(target);
        return (
          !savedDesktopSshEnvironmentKeys.has(target.alias) &&
          !savedDesktopSshEnvironmentKeys.has(address)
        );
      }),
    [discoveredSshHosts, savedDesktopSshEnvironmentKeys],
  );
  const hasLoadedDiscoveredSshHosts =
    desktopSshHosts.data !== null || desktopSshHosts.error !== null;
  const isLoadingDiscoveredSshHosts = desktopSshHosts.isPending;
  const discoveredSshHostsError = sshConnectionError ?? desktopSshHosts.error;
  const handleAddServer = useCallback(async () => {
    if (normalizedPairingCode === null) {
      setSavedBackendError("Enter a pairing code.");
      return;
    }
    if (requiresTunnelAcknowledgement && !tunnelAcknowledged) return;
    setIsAddingSavedBackend(true);
    setSavedBackendError(null);
    setAddServerFailure(null);
    const result = await connectRemoteServer({
      code: normalizedPairingCode,
      allowLoopbackTunnel: tunnelAcknowledged,
      ...(serverAlias.trim() ? { label: serverAlias.trim() } : {}),
    });
    setIsAddingSavedBackend(false);
    if (result._tag === "Failure") {
      if (isAtomCommandInterrupted(result)) return;
      const error = squashAtomCommandFailure(result);
      if (isLoopbackAcknowledgementRequired(error)) {
        setFlowDemandsAcknowledgement(true);
        return;
      }
      const reason = resolvePairingAddFailureReason(error);
      if (reason !== null) {
        setAddServerFailure({ reason, detail: resolvePairingAddFailureDetail(error) });
      } else {
        setSavedBackendError(error instanceof Error ? error.message : "Failed to add the server.");
      }
      return;
    }
    setPairingCodeInput("");
    setServerAlias("");
    setTunnelAcknowledged(false);
    setFlowDemandsAcknowledgement(false);
    setAddBackendDialogOpen(false);
    consumeInitialPairingCode();
    toastManager.add({
      type: "success",
      title: "Server added",
      description: "The server is saved and will reconnect on app startup.",
    });
  }, [
    connectRemoteServer,
    consumeInitialPairingCode,
    normalizedPairingCode,
    requiresTunnelAcknowledgement,
    serverAlias,
    tunnelAcknowledged,
  ]);
  const handleAddSavedBackend = useCallback(async () => {
    if (savedBackendMode === "ssh") {
      setIsAddingSavedBackend(true);
      setSavedBackendError(null);
      let target: DesktopSshEnvironmentTarget;
      try {
        target = parseManualDesktopSshTarget({
          host: savedBackendSshHost,
          username: savedBackendSshUsername,
          port: savedBackendSshPort,
        });
      } catch (error) {
        setSavedBackendError(formatDesktopSshConnectionError(error));
        setIsAddingSavedBackend(false);
        return;
      }

      // Captured before connecting: the host decides which environment this
      // is, and "updated" means it was already saved.
      const savedBefore = new Set(savedEnvironmentIds);
      const result = await connectSshEnvironment({ target, label: "" });
      if (result._tag === "Failure") {
        if (!isAtomCommandInterrupted(result)) {
          setSavedBackendError(formatDesktopSshConnectionError(squashAtomCommandFailure(result)));
        }
        setIsAddingSavedBackend(false);
        return;
      }

      setSavedBackendHost("");
      setSavedBackendPairingCode("");
      setSavedBackendSshHost("");
      setSavedBackendSshUsername("");
      setSavedBackendSshPort("");
      setAddBackendDialogOpen(false);
      consumeInitialPairingCode();
      toastManager.add({
        type: "success",
        ...describeSshEnvironmentAddedToast({
          label: target.alias,
          updated: savedBefore.has(result.value),
        }),
      });
      setIsAddingSavedBackend(false);
      return;
    }

    setIsAddingSavedBackend(true);
    setSavedBackendError(null);
    let remotePairingInput: ReturnType<typeof parseRemotePairingFields>;
    try {
      remotePairingInput = parseRemotePairingFields({
        host: savedBackendHost,
        pairingCode: savedBackendPairingCode,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to add backend.";
      setSavedBackendError(message);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not add backend",
          description: message,
        }),
      );
      setIsAddingSavedBackend(false);
      return;
    }

    const result = await connectPairing(remotePairingInput);
    if (result._tag === "Failure") {
      if (!isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = error instanceof Error ? error.message : "Failed to add backend.";
        setSavedBackendError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not add backend",
            description: message,
          }),
        );
      }
      setIsAddingSavedBackend(false);
      return;
    }

    setSavedBackendHost("");
    setSavedBackendPairingCode("");
    setSavedBackendSshHost("");
    setSavedBackendSshUsername("");
    setSavedBackendSshPort("");
    setAddBackendDialogOpen(false);
    consumeInitialPairingCode();
    toastManager.add({
      type: "success",
      title: "Backend added",
      description: "The environment is saved and will reconnect on app startup.",
    });
    setIsAddingSavedBackend(false);
  }, [
    connectPairing,
    connectSshEnvironment,
    consumeInitialPairingCode,
    savedBackendHost,
    savedBackendMode,
    savedBackendPairingCode,
    savedBackendSshHost,
    savedBackendSshPort,
    savedBackendSshUsername,
    savedEnvironmentIds,
  ]);

  const handleConnectSavedBackend = useCallback(
    async (environmentId: EnvironmentId) => {
      setSavedBackendError(null);
      const result = await connectEnvironment(environmentId);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = error instanceof Error ? error.message : "Failed to connect backend.";
        setSavedBackendError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not connect backend",
            description: message,
          }),
        );
      }
    },
    [connectEnvironment],
  );

  const handleDisconnectSavedBackend = useCallback(
    async (environmentId: EnvironmentId) => {
      setSavedBackendError(null);
      const result = await disconnectEnvironment(environmentId);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = error instanceof Error ? error.message : "Failed to disconnect backend.";
        setSavedBackendError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not disconnect backend",
            description: message,
          }),
        );
      }
    },
    [disconnectEnvironment],
  );

  const handleRemoveSavedBackend = useCallback(
    async (environmentId: EnvironmentId) => {
      setRemovingSavedEnvironmentId(environmentId);
      setSavedBackendError(null);
      const result = await removeEnvironment(environmentId);
      setRemovingSavedEnvironmentId(null);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = error instanceof Error ? error.message : "Failed to remove backend.";
        setSavedBackendError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not remove backend",
            description: message,
          }),
        );
      }
    },
    [removeEnvironment],
  );

  const handleRenameSavedBackend = useCallback(
    async (environmentId: EnvironmentId, label: string): Promise<string | null> => {
      const result = await renameEnvironment({ environmentId, label });
      if (result._tag === "Success" || isAtomCommandInterrupted(result)) return null;
      return describeRenameServerFailure(squashAtomCommandFailure(result));
    },
    [renameEnvironment],
  );
  const closeRenameDialog = useCallback(() => setRenameRequest(null), []);

  const handleConnectSshHost = useCallback(
    async (target: DesktopSshEnvironmentTarget, label?: string) => {
      setConnectingSshHostAlias(target.alias);
      if (savedBackendMode === "ssh") {
        setSavedBackendError(null);
      } else {
        setSshConnectionError(null);
      }
      const savedBefore = new Set(savedEnvironmentIds);
      const result = await connectSshEnvironment({
        target,
        ...(label === undefined ? {} : { label }),
      });
      setConnectingSshHostAlias(null);
      if (result._tag === "Success") {
        setSavedBackendSshHost("");
        setSavedBackendSshUsername("");
        setSavedBackendSshPort("");
        setAddBackendDialogOpen(false);
        consumeInitialPairingCode();
        toastManager.add({
          type: "success",
          ...describeSshEnvironmentAddedToast({
            label: label?.trim() || target.alias,
            updated: savedBefore.has(result.value),
          }),
        });
        return;
      }
      if (!isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = formatDesktopSshConnectionError(error);
        if (savedBackendMode === "ssh") {
          setSavedBackendError(message);
        } else {
          setSshConnectionError(message);
        }
      }
    },
    [connectSshEnvironment, consumeInitialPairingCode, savedBackendMode, savedEnvironmentIds],
  );
  const handleSavedBackendHostChange = useCallback((value: string) => {
    const resolution = tryResolveRemotePairingHostInput(value);
    if (resolution._tag === "Resolved") {
      setSavedBackendHost(resolution.host);
      setSavedBackendPairingCode(resolution.pairingCode);
      return;
    }
    setSavedBackendHost(value);
  }, []);

  const renderConnectionModeCard = (input: {
    readonly mode: "pairing-code" | "ssh";
    readonly title: string;
    readonly description: string;
    readonly icon?: ReactNode;
  }) => {
    const selected = savedBackendMode === input.mode;
    return (
      <button
        type="button"
        aria-pressed={selected}
        className={cn(
          "group flex min-h-24 items-start gap-3 rounded-lg border p-4 text-left",
          selected ? "border-primary/50 bg-primary/5" : "border-border/60 hover:bg-muted/40",
        )}
        disabled={isAddingSavedBackend}
        onClick={() => {
          setSavedBackendMode(input.mode);
        }}
      >
        {input.icon ? (
          <span
            className={cn(
              "mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md border",
              selected
                ? "border-primary/30 bg-primary/10 text-primary"
                : "border-border/70 bg-background text-muted-foreground group-hover:text-foreground",
            )}
          >
            {input.icon}
          </span>
        ) : null}
        <span className="min-w-0">
          <span className="block text-sm font-medium text-foreground">{input.title}</span>
          <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">
            {input.description}
          </span>
        </span>
      </button>
    );
  };

  const renderRemoteFields = () => (
    <div className="space-y-3">
      <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_10rem]">
        <label className="block">
          <span className="mb-1.5 block text-xs font-medium text-foreground">Host</span>
          <Input
            value={savedBackendHost}
            onChange={(event) => handleSavedBackendHostChange(event.target.value)}
            placeholder="backend.example.com"
            disabled={isAddingSavedBackend}
            spellCheck={false}
          />
        </label>
        <label className="block">
          <span className="mb-1.5 block text-xs font-medium text-foreground">Pairing code</span>
          <Input
            value={savedBackendPairingCode}
            onChange={(event) => setSavedBackendPairingCode(event.target.value)}
            placeholder="PAIRCODE"
            disabled={isAddingSavedBackend}
            spellCheck={false}
          />
        </label>
      </div>
      <div>
        <span className="mt-1 block text-xs text-muted-foreground">
          Paste a full pairing URL here to fill both fields automatically.
        </span>
      </div>
    </div>
  );
  const renderRemoteModeBody = () => (
    <div className="space-y-4">
      {renderRemoteFields()}
      {savedBackendError ? <p className="text-xs text-destructive">{savedBackendError}</p> : null}
      <Button
        variant="outline"
        className="w-full"
        disabled={isAddingSavedBackend}
        onClick={() => void handleAddSavedBackend()}
      >
        <PlusIcon className="size-3.5" />
        {isAddingSavedBackend ? "Adding…" : "Add manually"}
      </Button>
    </div>
  );
  const renderPairingCodeModeBody = () => {
    const describedFailure =
      addServerFailure === null
        ? null
        : describeAddServerFailure(addServerFailure.reason, addServerFailure.detail);
    return (
      <div className="space-y-4">
        <label className="block">
          <span className="mb-1.5 block text-xs font-medium text-foreground">
            Server alias (optional)
          </span>
          <Input
            value={serverAlias}
            onChange={(event) => setServerAlias(event.target.value)}
            placeholder="e.g. Linux workstation"
            disabled={isAddingSavedBackend}
          />
          <span className="mt-1 block text-xs text-muted-foreground">
            Shown on this device. Leave blank to use the server’s name.
          </span>
        </label>
        <label className="block">
          <span className="mb-1.5 block text-xs font-medium text-foreground">Pairing code</span>
          <Textarea
            value={pairingCodeInput}
            onChange={(event) => {
              setPairingCodeInput(event.target.value);
              setTunnelAcknowledged(false);
              setFlowDemandsAcknowledgement(false);
              setAddServerFailure(null);
              setSavedBackendError(null);
            }}
            placeholder="bibcode://pair?code=…"
            disabled={isAddingSavedBackend}
            spellCheck={false}
          />
        </label>
        {requiresTunnelAcknowledgement ? (
          <label className="flex items-start gap-3 rounded-md border border-warning/30 bg-warning/5 px-3 py-2.5 text-xs text-foreground">
            <Checkbox
              checked={tunnelAcknowledged}
              onCheckedChange={(checked) => setTunnelAcknowledged(checked === true)}
              disabled={isAddingSavedBackend}
            />
            <span>
              This address is only reachable on the server itself. I have set up a tunnel (SSH port
              forward or similar) from this device.
            </span>
          </label>
        ) : null}
        {describedFailure ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
            <p className="font-medium">{describedFailure.title}</p>
            <p className="mt-1">{describedFailure.detail}</p>
          </div>
        ) : savedBackendError ? (
          <p className="text-xs text-destructive">{savedBackendError}</p>
        ) : null}
        <Button
          className="w-full"
          disabled={isAddingSavedBackend || (requiresTunnelAcknowledgement && !tunnelAcknowledged)}
          onClick={() => void handleAddServer()}
        >
          <PlusIcon className="size-3.5" />
          {isAddingSavedBackend ? "Adding…" : "Add Server"}
        </Button>
        <Collapsible>
          <CollapsibleTrigger className="text-xs text-muted-foreground underline underline-offset-2">
            Advanced: manual endpoint and token
          </CollapsibleTrigger>
          <CollapsibleContent className="pt-3">{renderRemoteModeBody()}</CollapsibleContent>
        </Collapsible>
        <Collapsible>
          <CollapsibleTrigger className="text-xs text-muted-foreground underline underline-offset-2">
            Troubleshooting
          </CollapsibleTrigger>
          <CollapsibleContent className="pt-3">
            <ul className="space-y-2 text-xs text-muted-foreground">
              {ADD_SERVER_FAILURE_REASONS.map((reason) => {
                const described = describeAddServerFailure(reason);
                return (
                  <li key={reason}>
                    <span className="font-medium text-foreground">{described.title}.</span>{" "}
                    {described.detail}
                  </li>
                );
              })}
              <li>
                <span className="font-medium text-foreground">Still stuck?</span> Confirm both
                devices are on the same network or connected through a tunnel, then generate a fresh
                pairing code on the server&apos;s Share tab or with{" "}
                <code>bibcode pairing offer</code> on the server.
              </li>
            </ul>
          </CollapsibleContent>
        </Collapsible>
      </div>
    );
  };
  const renderSshFields = () => (
    <div className="space-y-4">
      <div className="space-y-3">
        <label className="block">
          <span className="mb-1.5 block text-xs font-medium text-foreground">
            SSH host or alias
          </span>
          <Input
            value={savedBackendSshHost}
            onChange={(event) => setSavedBackendSshHost(event.target.value)}
            placeholder="Search hosts or type devbox"
            disabled={isAddingSavedBackend}
            spellCheck={false}
          />
        </label>
        <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_7rem]">
          <label className="block">
            <span className="mb-1.5 block text-xs font-medium text-foreground">Username</span>
            <Input
              value={savedBackendSshUsername}
              onChange={(event) => setSavedBackendSshUsername(event.target.value)}
              placeholder="root"
              disabled={isAddingSavedBackend}
              spellCheck={false}
            />
          </label>
          <label className="block">
            <span className="mb-1.5 block text-xs font-medium text-foreground">Port</span>
            <Input
              value={savedBackendSshPort}
              onChange={(event) => setSavedBackendSshPort(event.target.value)}
              placeholder="22"
              inputMode="numeric"
              disabled={isAddingSavedBackend}
              spellCheck={false}
            />
          </label>
        </div>
        {savedBackendError || discoveredSshHostsError ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
            {savedBackendError ?? discoveredSshHostsError}
          </div>
        ) : null}
        <Button
          variant="outline"
          className="w-full"
          disabled={isAddingSavedBackend}
          onClick={() => void handleAddSavedBackend()}
        >
          <PlusIcon className="size-3.5" />
          {isAddingSavedBackend ? "Adding…" : "Add environment"}
        </Button>
      </div>
      <div className="overflow-hidden rounded-lg border border-border/60">
        <div className="flex items-center justify-between gap-3 border-b border-border/60 bg-muted/30 px-3 py-2">
          <div className="min-w-0">
            <p className="text-xs font-medium text-foreground">Suggested hosts</p>
            <p className="text-xs text-muted-foreground">From SSH config and known hosts</p>
          </div>
          <Button
            size="xs"
            variant="ghost"
            disabled={isLoadingDiscoveredSshHosts}
            onClick={desktopSshHosts.refresh}
          >
            {isLoadingDiscoveredSshHosts ? (
              <RefreshCwIcon className="size-3 animate-spin" />
            ) : (
              <RefreshCwIcon className="size-3" />
            )}
            Refresh
          </Button>
        </div>
        <ScrollArea scrollFade className="max-h-56">
          <div>
            {unsavedDiscoveredSshHosts.map((target) => (
              <DesktopSshHostRow
                key={`${target.alias}:${target.hostname}:${target.port ?? ""}`}
                target={target}
                connectingHostAlias={connectingSshHostAlias}
                onConnect={(nextTarget) => void handleConnectSshHost(nextTarget)}
              />
            ))}
            {hasLoadedDiscoveredSshHosts &&
            !isLoadingDiscoveredSshHosts &&
            unsavedDiscoveredSshHosts.length === 0 ? (
              <div className={ITEM_ROW_CLASSNAME}>
                <p className="text-xs text-muted-foreground">No new SSH hosts were discovered.</p>
              </div>
            ) : null}
          </div>
        </ScrollArea>
      </div>
    </div>
  );
  return (
    <>
      <SettingsSection
        title="Saved servers"
        headerAction={
          <div className="flex items-center gap-1">
            {showServerUpdateCheck && updateCapableEnvironmentIds.length > 0 ? (
              <Button
                size="xs"
                variant="ghost"
                className="h-5 gap-1 rounded-sm px-1 text-xs font-normal text-muted-foreground hover:text-foreground"
                disabled={checkingAllServerUpdates}
                onClick={() => void onCheckForServerUpdates()}
              >
                <RefreshCwIcon
                  className={cn("size-3", checkingAllServerUpdates && "animate-spin")}
                />
                <span>
                  {checkingAllServerUpdates
                    ? "Checking Server Updates…"
                    : "Check for Server Updates"}
                </span>
              </Button>
            ) : null}
            <Dialog
              open={addBackendDialogOpen}
              onOpenChange={(open) => {
                setAddBackendDialogOpen(open);
                if (!open) {
                  setSavedBackendError(null);
                  consumeInitialPairingCode();
                }
              }}
            >
              <Tooltip>
                <TooltipTrigger
                  render={
                    <DialogTrigger
                      render={
                        <Button
                          size="xs"
                          variant="ghost"
                          className="h-5 gap-1 rounded-sm px-1 text-xs font-normal text-muted-foreground hover:text-foreground"
                          aria-label="Add Server"
                        >
                          <PlusIcon className="size-3" />
                          <span>Add Server</span>
                        </Button>
                      }
                    />
                  }
                />
                <TooltipPopup side="top">Add Server</TooltipPopup>
              </Tooltip>
              <DialogPopup className="max-h-[80dvh] sm:max-w-3xl">
                <DialogHeader>
                  <DialogTitle>Add Server</DialogTitle>
                  <DialogDescription>
                    Connect this device to another BiBCode server.
                  </DialogDescription>
                </DialogHeader>
                <DialogPanel>
                  <div className="space-y-4">
                    <div className="grid gap-3 sm:grid-cols-2">
                      {renderConnectionModeCard({
                        mode: "pairing-code",
                        title: "Pairing code",
                        description:
                          "Paste a pairing code from the server's Share tab or its bibcode pairing offer command.",
                        icon: <QrCodeIcon aria-hidden className="size-4" />,
                      })}
                      {desktopBridge
                        ? renderConnectionModeCard({
                            mode: "ssh",
                            title: "SSH",
                            description:
                              "Use local SSH config, agent, and tunnels for the backend.",
                            icon: <TerminalIcon aria-hidden className="size-4" />,
                          })
                        : null}
                    </div>
                    <AnimatedHeight>
                      {savedBackendMode === "ssh" ? renderSshFields() : renderPairingCodeModeBody()}
                    </AnimatedHeight>
                  </div>
                </DialogPanel>
              </DialogPopup>
            </Dialog>
          </div>
        }
      >
        {savedEnvironments.map((environment) => (
          <RemoteServerRowFromSession
            key={environment.environmentId}
            environment={environment}
            updateRefreshEpoch={updateRefreshEpochs.get(environment.environmentId) ?? 0}
            removingEnvironmentId={removingSavedEnvironmentId}
            onConnect={handleConnectSavedBackend}
            onDisconnect={handleDisconnectSavedBackend}
            onRequestRemove={(environmentId, label) =>
              setRemovalCandidate({ environmentId, label })
            }
            onRequestRename={setRenameRequest}
          />
        ))}
        <CloudRemoteEnvironmentRows
          primaryEnvironmentId={primaryEnvironmentId}
          savedEnvironmentIds={savedEnvironmentIds}
        />
      </SettingsSection>
      <AlertDialog
        open={removalCandidate !== null}
        onOpenChange={(open) => {
          if (!open) setRemovalCandidate(null);
        }}
      >
        <AlertDialogPopup>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {removalCandidate ? `Remove ${removalCandidate.label}?` : ""}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {runningRemovalCount > 0
                ? `${runningRemovalCount} running ${runningRemovalCount === 1 ? "session" : "sessions"} on ${removalCandidate?.label} will keep running on the server but disappear from this device until you pair again. Removing deletes the saved server and its credentials from this device.`
                : "This deletes the saved server and its credentials from this device. The server itself is not affected."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogClose render={<Button variant="outline" />}>Cancel</AlertDialogClose>
            <Button
              variant="destructive"
              disabled={removingSavedEnvironmentId !== null}
              onClick={() => {
                if (removalCandidate === null) return;
                const environmentId = removalCandidate.environmentId;
                setRemovalCandidate(null);
                void handleRemoveSavedBackend(environmentId);
              }}
            >
              Remove server
            </Button>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
      <RenameServerDialog
        request={renameRequest}
        onClose={closeRenameDialog}
        onRename={handleRenameSavedBackend}
      />
    </>
  );
}
