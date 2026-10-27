import {
  EnvironmentId,
  type DesktopBackendRecovery,
  type DesktopBridge,
  type DesktopUpdateActionResult,
  type DesktopUpdateProtection,
  type DesktopUpdateState,
} from "@coddy/contracts";
import { runAtomCommand } from "@coddy/client-runtime/state/runtime";
import { useEffect, useId, useRef, useState } from "react";

import { appAtomRegistry } from "../../rpc/atomRegistry";
import {
  getDesktopBackendRecoveryMessage,
  getDesktopUpdateErrorMessage,
} from "../desktopUpdate.logic";
import { Button } from "../ui/button";
import { Checkbox } from "../ui/checkbox";
import { stackedThreadToast, toastManager } from "../ui/toast";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "../ui/dialog";

type InstallUpdate = Pick<DesktopBridge, "installUpdate">["installUpdate"];

// Dialog instances and their retained toast actions share native operation admission.
let desktopUpdateActionInFlight = false;
let recoveryToastId: ReturnType<typeof toastManager.add> | null = null;

function closeRecoveryToast(): void {
  if (recoveryToastId === null) return;
  toastManager.close(recoveryToastId);
  recoveryToastId = null;
}

function addRecoveryToast(toast: Parameters<typeof toastManager.add>[0]): void {
  closeRecoveryToast();
  recoveryToastId = toastManager.add(toast);
}

async function runDesktopUpdateAction(
  setBusy: (busy: boolean) => void,
  action: () => Promise<void>,
): Promise<void> {
  if (desktopUpdateActionInFlight) return;
  desktopUpdateActionInFlight = true;
  try {
    setBusy(true);
    await action();
  } finally {
    desktopUpdateActionInFlight = false;
    setBusy(false);
  }
}

async function retryEnvironment(environmentId: string): Promise<void> {
  try {
    const { environmentAvailabilityCommands } = await import("../../state/shell");
    await runAtomCommand(
      appAtomRegistry,
      environmentAvailabilityCommands.retry,
      EnvironmentId.make(environmentId),
      { label: environmentAvailabilityCommands.retry.label, reportFailure: false },
    );
  } catch {
    // Reconnection is best-effort and must also work from a toast after unmount.
  }
}

interface UpdateProtectionDialogProps {
  readonly open: boolean;
  readonly state: DesktopUpdateState;
  readonly onOpenChange: (open: boolean) => void;
  readonly installUpdate: InstallUpdate;
  readonly onDiagnostics: () => void;
  readonly onError?: (message: string) => void;
}

function protectionLabel(entry: DesktopUpdateProtection, protecting: boolean): string {
  switch (entry.status) {
    case "pending":
      return protecting ? `Protecting ${entry.label}` : `Waiting to protect ${entry.label}`;
    case "protected":
      return `Protected ${entry.label}`;
    case "failed":
      return `Could not protect ${entry.label}`;
    case "excluded":
      return `Excluded ${entry.label}`;
    case "skipped":
      return `Skipped backup for ${entry.label}`;
  }
}

function protectionProgress(entry: DesktopUpdateProtection): string | null {
  if (entry.stage === undefined || entry.stage === null) return null;
  const stage = (() => {
    switch (entry.stage) {
      case "waiting-for-mutations":
        return "Waiting for active operations";
      case "quiescing-runtime":
        return "Stopping active tasks";
      case "acquiring-store-lock":
        return "Preparing the project database";
      case "checkpointing-database":
        return "Checkpointing the project database";
      case "creating-verified-backup":
        return "Creating and verifying the backup";
      case "stopping-backend":
        return "Stopping the local backend";
    }
  })();
  const details = [stage];
  if (entry.blockedOperationCount !== undefined && entry.blockedOperationCount !== null) {
    details.push(
      `${entry.blockedOperationCount} active operation${entry.blockedOperationCount === 1 ? "" : "s"}`,
    );
  }
  if (entry.elapsedMs !== undefined && entry.elapsedMs !== null) {
    details.push(`${Math.floor(entry.elapsedMs / 1_000)}s elapsed`);
  }
  return details.join(" · ");
}

export function UpdateProtectionDialog({
  open,
  state,
  onOpenChange,
  installUpdate,
  onDiagnostics,
  onError,
}: UpdateProtectionDialogProps) {
  const [excludedEnvironmentIds, setExcludedEnvironmentIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [submitting, setSubmitting] = useState(false);
  const [skipProtectionAcknowledged, setSkipProtectionAcknowledged] = useState(false);
  const [restarting, setRestarting] = useState<"server" | "app" | null>(null);
  const [restartError, setRestartError] = useState<string | null>(null);
  const [appRestartError, setAppRestartError] = useState<string | null>(null);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const latestState = useRef(state);
  const retryReasonId = useId();
  const detailsId = useId();
  const bridge = typeof window === "undefined" ? undefined : window.desktopBridge;
  const recovery = state.backendRecovery ?? [];
  const recovering = recovery.length > 0;
  const canRestartApp = restartError !== null && bridge?.restartApp !== undefined;
  const protection = state.protection ?? [];
  const phase = state.phase ?? "idle";
  const protecting = phase === "protecting";
  const choosingProtection = open && !protecting;
  const [wasChoosingProtection, setWasChoosingProtection] = useState(choosingProtection);
  const working = protecting || phase === "installing" || submitting || restarting !== null;
  const primaryFailure = protection.find(
    (entry) => entry.environmentId === "primary" && entry.status === "failed",
  );
  const failedSecondaries = protection.filter(
    (entry) => entry.environmentId !== "primary" && entry.status === "failed",
  );
  const hasProtectionFailure = primaryFailure !== undefined || failedSecondaries.length > 0;
  const exclusionsComplete = failedSecondaries.every((entry) =>
    excludedEnvironmentIds.has(entry.environmentId),
  );

  useEffect(() => {
    latestState.current = state;
  }, [state]);

  useEffect(() => {
    if (!recovering) closeRecoveryToast();
  }, [recovering]);

  if (wasChoosingProtection !== choosingProtection) {
    setWasChoosingProtection(choosingProtection);
    if (!choosingProtection) {
      setExcludedEnvironmentIds(new Set());
      setSkipProtectionAcknowledged(false);
    }
  }

  const installLabel = (() => {
    if (restarting === "server") return "Restarting server…";
    if (restarting === "app") return "Restarting Coddy Studio…";
    if (working) return protecting ? "Protecting projects…" : "Installing update…";
    if (primaryFailure) return "Retry protection";
    if (failedSecondaries.length > 0) return "Install with exclusions";
    if (phase === "failed") return "Retry installation";
    return "Protect projects and install";
  })();

  const restartCoddyStudio = async (fromToast = false) => {
    const restartApp = bridge?.restartApp;
    if (!restartApp) return;
    await runDesktopUpdateAction(
      (busy) => setRestarting(busy ? "app" : null),
      async () => {
        setAppRestartError(null);
        try {
          await restartApp();
        } catch (error) {
          const message = getDesktopUpdateErrorMessage(
            error,
            "Coddy Studio couldn't restart. Try again.",
          );
          setAppRestartError(message);
          if (fromToast) {
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Could not restart Coddy Studio",
                description: message,
              }),
            );
          }
        }
      },
    );
  };

  const showRecoveryToast = (entries: readonly DesktopBackendRecovery[], retryFailed = false) => {
    const actionProps = retryFailed
      ? bridge?.restartApp
        ? { children: "Restart Coddy Studio", onClick: () => void restartCoddyStudio(true) }
        : undefined
      : { children: "Restart server", onClick: () => void restartServers(entries, true) };
    addRecoveryToast(
      stackedThreadToast({
        type: "error",
        title: "Update not installed",
        timeout: 0,
        description: entries.map(getDesktopBackendRecoveryMessage).join("\n"),
        ...(actionProps ? { actionProps } : {}),
      }),
    );
  };

  const restartServers = async (entries: readonly DesktopBackendRecovery[], fromToast = false) => {
    await runDesktopUpdateAction(
      (busy) => setRestarting(busy ? "server" : null),
      async () => {
        setRestartError(null);
        setAppRestartError(null);
        const errors: string[] = [];
        if (!bridge?.retryProjectData) {
          errors.push("Restart server is unavailable. Restart Coddy Studio and try again.");
        } else {
          // The native host admits only one project-data operation at a time.
          for (const entry of entries) {
            try {
              await bridge.retryProjectData(entry.environmentId);
              void retryEnvironment(entry.environmentId);
            } catch (error) {
              errors.push(
                getDesktopUpdateErrorMessage(error, "Coddy Studio's local server couldn't restart."),
              );
            }
          }
        }
        setRestartError(errors.length > 0 ? errors.join("\n") : null);
        if (errors.length === 0) closeRecoveryToast();
        if (fromToast) {
          if (errors.length === 0) {
            toastManager.add({ type: "success", title: "Server restarted" });
          } else {
            // The supervisor publishes recovery before the retry returns; read its latest classification.
            const settled = await bridge?.getUpdateState().catch(() => latestState.current);
            const currentEntries = settled?.backendRecovery ?? latestState.current.backendRecovery;
            showRecoveryToast(currentEntries?.length ? currentEntries : entries, true);
          }
        }
      },
    );
  };

  const handleInstallResult = (result: DesktopUpdateActionResult) => {
    if (result.completed) return;
    if (result.state.backendRecovery?.length) {
      showRecoveryToast(result.state.backendRecovery);
    } else if (result.state.message) {
      onError?.(result.state.message);
    }
  };

  const startInstall = async () => {
    if (working || recovering || (failedSecondaries.length > 0 && !exclusionsComplete)) return;
    await runDesktopUpdateAction(setSubmitting, async () => {
      setRestartError(null);
      setAppRestartError(null);
      setDetailsOpen(false);
      try {
        const exclusions = [...excludedEnvironmentIds];
        const result = await installUpdate(
          exclusions.length > 0 ? { excludedEnvironmentIds: exclusions } : undefined,
        );
        handleInstallResult(result);
      } catch (error) {
        onError?.(error instanceof Error ? error.message : "An unexpected error occurred.");
      }
    });
  };

  const startUnprotectedInstall = async () => {
    if (working || recovering || !hasProtectionFailure || !skipProtectionAcknowledged) return;
    await runDesktopUpdateAction(setSubmitting, async () => {
      setRestartError(null);
      setAppRestartError(null);
      setDetailsOpen(false);
      try {
        const result = await installUpdate({ skipProtection: true });
        handleInstallResult(result);
      } catch (error) {
        onError?.(error instanceof Error ? error.message : "An unexpected error occurred.");
      }
    });
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!working && !desktopUpdateActionInFlight) onOpenChange(nextOpen);
      }}
    >
      <DialogPopup className="max-w-lg" showCloseButton={!working}>
        <DialogHeader>
          <DialogTitle>
            {recovering ? "Update not installed" : "Protect projects before updating"}
          </DialogTitle>
          <DialogDescription>
            {recovering
              ? recovery.map((entry) => (
                  <span key={entry.environmentId} className="block">
                    {getDesktopBackendRecoveryMessage(entry)}
                  </span>
                ))
              : "Coddy Studio creates a verified project database backup by default before stopping each included local backend. Running tasks will be interrupted."}
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-3" data-text-surface="card">
          {recovering ? (
            <>
              <Button
                variant="outline"
                disabled={working}
                aria-expanded={detailsOpen}
                aria-controls={detailsId}
                onClick={() => setDetailsOpen((current) => !current)}
              >
                Details
              </Button>
              {detailsOpen ? (
                <div id={detailsId} className="max-h-48 overflow-y-auto rounded-md border">
                  <p
                    data-text-surface="card"
                    className="whitespace-pre-wrap break-words p-3 text-xs"
                  >
                    {restartError ?? state.message}
                  </p>
                </div>
              ) : null}
              {appRestartError ? (
                <p role="alert" className="text-sm text-destructive">
                  {appRestartError}
                </p>
              ) : null}
            </>
          ) : protection.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              The primary project store will be protected before installation begins.
            </p>
          ) : (
            <ul className="space-y-2">
              {protection.map((entry) => (
                <li key={entry.environmentId} className="rounded-lg border p-3 text-sm">
                  <div className="font-medium">{protectionLabel(entry, protecting)}</div>
                  {entry.message ? (
                    <p className="mt-1 text-xs text-muted-foreground">{entry.message}</p>
                  ) : null}
                  {entry.status === "pending" && protectionProgress(entry) ? (
                    <p className="mt-1 text-xs text-muted-foreground">
                      {protectionProgress(entry)}
                    </p>
                  ) : null}
                  {entry.status === "failed" && entry.environmentId !== "primary" ? (
                    <label className="mt-2 flex items-center gap-2">
                      <Checkbox
                        aria-label={`Exclude ${entry.label}`}
                        checked={excludedEnvironmentIds.has(entry.environmentId)}
                        disabled={working}
                        onCheckedChange={(checked) => {
                          setExcludedEnvironmentIds((current) => {
                            const next = new Set(current);
                            if (checked) next.add(entry.environmentId);
                            else next.delete(entry.environmentId);
                            return next;
                          });
                        }}
                      />
                      <span>Exclude {entry.label} from this update</span>
                    </label>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
          {!recovering && hasProtectionFailure ? (
            <div
              aria-label="Continue without a backup"
              className="rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-sm"
              role="group"
            >
              <p className="font-medium">Continue without a backup</p>
              <p className="mt-1 text-xs text-muted-foreground">
                This update will stop local backends without creating a verified rollback backup.
              </p>
              <label className="mt-2 flex items-center gap-2">
                <Checkbox
                  aria-label="Acknowledge update without backup"
                  checked={skipProtectionAcknowledged}
                  disabled={working}
                  onCheckedChange={setSkipProtectionAcknowledged}
                />
                <span>I understand that this update will not create a backup</span>
              </label>
              <div className="mt-3 flex justify-end">
                <Button
                  className="w-full sm:w-auto"
                  variant="destructive"
                  disabled={working || !skipProtectionAcknowledged}
                  onClick={() => void startUnprotectedInstall()}
                >
                  Install without backup
                </Button>
              </div>
            </div>
          ) : null}
        </DialogPanel>
        <DialogFooter>
          {recovering ? (
            <div className="flex w-full flex-col gap-3">
              {restartError !== null ? (
                <p role="alert" className="text-sm text-destructive">
                  {recovery.length === 1
                    ? "The server still couldn't restart."
                    : "The servers still couldn't restart."}
                </p>
              ) : null}
              <p id={retryReasonId} className="text-xs text-muted-foreground">
                Restart the server before retrying the installation.
              </p>
              <div className="flex flex-wrap justify-end gap-2">
                <Button variant="outline" disabled={working} onClick={() => onOpenChange(false)}>
                  Close
                </Button>
                <Button variant="outline" disabled aria-describedby={retryReasonId}>
                  Retry installation
                </Button>
                <Button
                  variant={canRestartApp ? "outline" : "default"}
                  disabled={working}
                  onClick={() => void restartServers(recovery)}
                >
                  {restarting === "server" ? "Restarting server…" : "Restart server"}
                </Button>
                {canRestartApp ? (
                  <Button disabled={working} onClick={() => void restartCoddyStudio()}>
                    {restarting === "app" ? "Restarting Coddy Studio…" : "Restart Coddy Studio"}
                  </Button>
                ) : null}
              </div>
            </div>
          ) : (
            <>
              <Button variant="outline" disabled={working} onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              {primaryFailure ? (
                <Button variant="outline" disabled={working} onClick={onDiagnostics}>
                  Diagnostics
                </Button>
              ) : null}
              <Button
                disabled={working || (failedSecondaries.length > 0 && !exclusionsComplete)}
                onClick={() => void startInstall()}
              >
                {installLabel}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
