import { Link } from "@tanstack/react-router";
import { type ReactElement, useCallback, useMemo, useState } from "react";
import type { DesktopWslState } from "@coddy/contracts";

import { isDesktopLocalConnectionTarget } from "~/connection/desktopLocal";
import { desktopWslStateAtom, refreshDesktopWslState } from "~/state/desktopWslState";
import { useEnvironments } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import { applyWslEnableSelection } from "./ConnectionsSettings.logic";
import { SettingsPageContainer, SettingsRow, SettingsSection } from "./settingsLayout";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../ui/alert-dialog";
import { Button } from "../ui/button";
import { RetryButton } from "../ui/retry-button";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { Spinner } from "../ui/spinner";
import { Switch } from "../ui/switch";
import { stackedThreadToast, toastManager } from "../ui/toast";

// Colons cannot collide with a validated WSL distro name.
const BACKEND_VALUE_DEFAULT_WSL = "backend:default-wsl";
const BACKEND_VALUE_WSL_OFF = "backend:wsl-off";

type PendingWslChange =
  | { readonly kind: "disable"; readonly wasWslOnly: boolean }
  | { readonly kind: "distro"; readonly nextDistro: string | null }
  | { readonly kind: "enable"; readonly nextDistro: string | null }
  | { readonly kind: "wsl-only"; readonly nextValue: boolean };

export function LocalEnvironmentSettings(): ReactElement {
  const desktopBridge = window.desktopBridge;
  const { environments } = useEnvironments();
  const [isUpdatingWslBackend, setIsUpdatingWslBackend] = useState(false);
  const [desktopWslMutationError, setDesktopWslMutationError] = useState<string | null>(null);
  const [pendingWslChange, setPendingWslChange] = useState<PendingWslChange | null>(null);
  const desktopWsl = useEnvironmentQuery(desktopBridge ? desktopWslStateAtom : null);
  const desktopWslState = desktopWsl.data;
  const desktopWslError = desktopWslMutationError ?? desktopWsl.error;
  const isLoadingWslState = desktopWsl.isPending && desktopWsl.data === null;

  const applyWslSettingChange = useCallback(
    async (apply: () => Promise<DesktopWslState>) => {
      if (!desktopBridge) return;
      setIsUpdatingWslBackend(true);
      setDesktopWslMutationError(null);
      try {
        await apply();
        refreshDesktopWslState();
      } catch (error) {
        const message = error instanceof Error ? error.message : "Failed to update WSL backend.";
        setDesktopWslMutationError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not change WSL backend",
            description: message,
          }),
        );
        refreshDesktopWslState();
      } finally {
        setIsUpdatingWslBackend(false);
      }
    },
    [desktopBridge],
  );

  const loadWslState = useCallback(() => {
    setDesktopWslMutationError(null);
    refreshDesktopWslState();
  }, []);

  const retryWslPrimary = useCallback(() => {
    if (!desktopBridge || !desktopWslState) return;
    void applyWslSettingChange(() => desktopBridge.setWslDistro(desktopWslState.distro));
  }, [applyWslSettingChange, desktopBridge, desktopWslState]);

  const switchPrimaryToWindows = useCallback(() => {
    if (!desktopBridge || !desktopWslState?.wslOnly) return;
    void applyWslSettingChange(() => desktopBridge.setWslBackendEnabled(false));
  }, [applyWslSettingChange, desktopBridge, desktopWslState]);

  const turnOffWslSecondary = useCallback(() => {
    if (!desktopBridge || !desktopWslState?.enabled || desktopWslState.wslOnly) return;
    void applyWslSettingChange(() => desktopBridge.setWslBackendEnabled(false));
  }, [applyWslSettingChange, desktopBridge, desktopWslState]);

  const hasWslRegistrationToLose = useMemo(
    () =>
      environments.some((environment) => isDesktopLocalConnectionTarget(environment.entry.target)),
    [environments],
  );

  const handleSelectWslMode = useCallback(
    (value: string) => {
      if (!desktopBridge || !desktopWslState) return;
      const defaultDistroName =
        desktopWslState.distros.find((distro) => distro.isDefault)?.name ?? null;
      if (value === BACKEND_VALUE_WSL_OFF) {
        if (!desktopWslState.enabled && !desktopWslState.wslOnly) return;
        const wasWslOnly = desktopWslState.wslOnly;
        if (hasWslRegistrationToLose || wasWslOnly) {
          setPendingWslChange({ kind: "disable", wasWslOnly });
          return;
        }
        void applyWslSettingChange(() => desktopBridge.setWslBackendEnabled(false));
        return;
      }
      const nextDistro = value === BACKEND_VALUE_DEFAULT_WSL ? null : value;
      const resolvedNext = nextDistro ?? defaultDistroName;
      if (!desktopWslState.enabled) {
        setPendingWslChange({ kind: "enable", nextDistro });
        return;
      }
      const resolvedCurrent = desktopWslState.distro ?? defaultDistroName;
      if (resolvedCurrent === resolvedNext) return;
      if (hasWslRegistrationToLose || desktopWslState.wslOnly) {
        setPendingWslChange({ kind: "distro", nextDistro });
        return;
      }
      void applyWslSettingChange(() => desktopBridge.setWslDistro(nextDistro));
    },
    [applyWslSettingChange, desktopBridge, desktopWslState, hasWslRegistrationToLose],
  );

  const handleConfirmEnableWsl = useCallback(
    (mode: "both" | "wsl-only") => {
      if (!desktopBridge || !pendingWslChange || pendingWslChange.kind !== "enable") return;
      const nextDistro = pendingWslChange.nextDistro;
      setPendingWslChange(null);
      const persistedDistro = desktopWslState?.distro ?? null;
      void applyWslSettingChange(() =>
        applyWslEnableSelection({
          bridge: desktopBridge,
          mode,
          nextDistro,
          persistedDistro,
        }),
      );
    },
    [applyWslSettingChange, desktopBridge, desktopWslState, pendingWslChange],
  );

  const handleToggleWslOnly = useCallback(
    (enabled: boolean) => {
      if (!desktopBridge || !desktopWslState || desktopWslState.wslOnly === enabled) return;
      setPendingWslChange({ kind: "wsl-only", nextValue: enabled });
    },
    [desktopBridge, desktopWslState],
  );

  const handleConfirmWslChange = useCallback(() => {
    if (!desktopBridge || !pendingWslChange) return;
    const change = pendingWslChange;
    if (change.kind === "enable") return;
    setPendingWslChange(null);
    if (change.kind === "disable") {
      void applyWslSettingChange(() => desktopBridge.setWslBackendEnabled(false));
      return;
    }
    if (change.kind === "distro") {
      void applyWslSettingChange(() => desktopBridge.setWslDistro(change.nextDistro));
      return;
    }
    void applyWslSettingChange(() => desktopBridge.setWslOnly(change.nextValue));
  }, [applyWslSettingChange, desktopBridge, pendingWslChange]);

  const renderWslRow = () => {
    if (!desktopBridge) {
      return (
        <SettingsRow
          title="WSL backend unavailable"
          description="Desktop integration is unavailable. Restart Coddy Studio to manage the local WSL backend."
          status={
            <span role="alert" className="block text-destructive">
              Desktop bridge unavailable
            </span>
          }
        />
      );
    }

    if (!desktopWslState) {
      if (desktopWslError) {
        return (
          <SettingsRow
            title="WSL backend"
            description="Couldn't load the WSL backend state."
            status={<span className="block text-destructive">{desktopWslError}</span>}
            control={<RetryButton retrying={isLoadingWslState} onRetry={loadWslState} />}
          />
        );
      }
      if (isLoadingWslState) {
        return (
          <SettingsRow
            title="WSL backend"
            description="Loading local WSL backend configuration."
            status={
              <span role="status" aria-live="polite" className="flex items-center gap-1.5">
                <Spinner className="size-3.5" />
                Loading WSL backend settings…
              </span>
            }
          />
        );
      }
      return (
        <SettingsRow
          title="WSL backend"
          description="Couldn't load the WSL backend state."
          status={
            <span role="alert" className="block text-destructive">
              WSL backend state unavailable
            </span>
          }
          control={
            <Button size="xs" variant="outline" onClick={loadWslState}>
              Retry
            </Button>
          }
        />
      );
    }

    if (
      !desktopWslState.available ||
      desktopWslState.preflightError?.kind === "wsl-secondary-unavailable"
    ) {
      if (!desktopWslState.enabled && !desktopWslState.wslOnly) {
        return (
          <SettingsRow
            title="WSL backend"
            description="WSL is unavailable. Install or enable WSL and a Linux distribution, then retry."
            status={
              <span role="alert" className="block text-destructive">
                WSL backend unavailable
              </span>
            }
            control={
              <Button size="xs" variant="outline" onClick={loadWslState}>
                Retry
              </Button>
            }
          />
        );
      }
      const isPrimaryWslFailure =
        desktopWslState.wslOnly ||
        desktopWslState.preflightError?.kind === "wsl-primary-unavailable";
      return (
        <SettingsRow
          title="WSL backend"
          description={
            isPrimaryWslFailure
              ? "WSL is unavailable and no Windows backend was substituted. Retry WSL or explicitly switch primary execution to Windows."
              : "The Windows backend remains primary, but the configured WSL secondary is unavailable."
          }
          status={
            desktopWslError ? (
              <span className="block text-destructive">{desktopWslError}</span>
            ) : desktopWslState.preflightError ? (
              <span className="block text-destructive">
                WSL backend couldn't start: {desktopWslState.preflightError.detail}
              </span>
            ) : null
          }
          control={
            <div className="flex flex-wrap justify-end gap-1">
              <Button
                size="xs"
                variant="outline"
                disabled={isUpdatingWslBackend}
                onClick={retryWslPrimary}
              >
                Retry WSL
              </Button>
              <Button render={<Link to="/settings/diagnostics" />} size="xs" variant="ghost">
                View diagnostics
              </Button>
              <Button
                size="xs"
                variant="outline"
                disabled={isUpdatingWslBackend}
                onClick={isPrimaryWslFailure ? switchPrimaryToWindows : turnOffWslSecondary}
              >
                {isPrimaryWslFailure ? "Switch to Windows" : "Turn off WSL"}
              </Button>
            </div>
          }
        />
      );
    }

    const defaultDistroName =
      desktopWslState.distros.find((distro) => distro.isDefault)?.name ?? null;
    const selectValue = !desktopWslState.enabled
      ? BACKEND_VALUE_WSL_OFF
      : (desktopWslState.distro ?? defaultDistroName ?? BACKEND_VALUE_DEFAULT_WSL);
    const selectLabel =
      selectValue === BACKEND_VALUE_WSL_OFF
        ? "Off"
        : selectValue === BACKEND_VALUE_DEFAULT_WSL
          ? "Default distro"
          : selectValue;
    return (
      <>
        <SettingsRow
          title="WSL backend"
          description={
            desktopWslState.preflightError?.kind === "wsl-primary-unavailable"
              ? "WSL is unavailable and no Windows backend was substituted."
              : "Run a second backend inside a WSL distro alongside the Windows one. Pick a distro to start it; pick Off to stop it. Projects opened against the WSL backend live on the Linux side; Windows projects stay where they are."
          }
          status={
            desktopWslError ? (
              <span className="block text-destructive">{desktopWslError}</span>
            ) : desktopWslState.preflightError?.kind === "wsl-primary-unavailable" ? (
              <span className="block text-destructive">
                WSL backend couldn't start: {desktopWslState.preflightError.detail}
              </span>
            ) : null
          }
          control={
            <Select
              value={selectValue}
              onValueChange={(value) => {
                if (typeof value !== "string") return;
                handleSelectWslMode(value);
              }}
            >
              <SelectTrigger
                className="w-full sm:w-56"
                aria-label="WSL backend"
                disabled={isUpdatingWslBackend}
              >
                <SelectValue>{selectLabel}</SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                <SelectItem hideIndicator value={BACKEND_VALUE_WSL_OFF}>
                  Off
                </SelectItem>
                {desktopWslState.distros.length === 0 ? (
                  <SelectItem hideIndicator value={BACKEND_VALUE_DEFAULT_WSL}>
                    Default distro
                  </SelectItem>
                ) : (
                  desktopWslState.distros.map((distro) => (
                    <SelectItem hideIndicator key={distro.name} value={distro.name}>
                      {distro.name}
                      {distro.isDefault ? " (default)" : ""}
                    </SelectItem>
                  ))
                )}
              </SelectPopup>
            </Select>
          }
        />
        {desktopWslState.preflightError?.kind === "wsl-primary-unavailable" ? (
          <SettingsRow
            title="WSL recovery"
            description="Retry the selected distro, inspect diagnostics, or explicitly switch primary execution to Windows."
            className="bg-muted/20 pl-7 sm:pl-8"
            control={
              <div className="flex flex-wrap justify-end gap-1">
                <Button
                  size="xs"
                  variant="outline"
                  disabled={isUpdatingWslBackend}
                  onClick={retryWslPrimary}
                >
                  Retry WSL
                </Button>
                <Button render={<Link to="/settings/diagnostics" />} size="xs" variant="ghost">
                  View diagnostics
                </Button>
                <Button
                  size="xs"
                  variant="outline"
                  disabled={isUpdatingWslBackend}
                  onClick={switchPrimaryToWindows}
                >
                  Switch to Windows
                </Button>
              </div>
            }
          />
        ) : null}
        {desktopWslState.enabled ? (
          <SettingsRow
            title="WSL only"
            description="Stop the Windows backend and run only the WSL backend. Useful if you develop entirely inside WSL and don't want a second backend process. Coddy Studio restarts when you change this."
            className="bg-muted/20 pl-7 sm:pl-8"
            control={
              <Switch
                checked={desktopWslState.wslOnly}
                disabled={isUpdatingWslBackend}
                onCheckedChange={handleToggleWslOnly}
                aria-label="Run WSL only"
              />
            }
          />
        ) : null}
      </>
    );
  };

  return (
    <SettingsPageContainer>
      <SettingsSection title="Local environment">{renderWslRow()}</SettingsSection>
      <AlertDialog
        open={pendingWslChange !== null}
        onOpenChange={(open) => {
          if (isUpdatingWslBackend) return;
          if (!open) setPendingWslChange(null);
        }}
      >
        <AlertDialogPopup>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {pendingWslChange?.kind === "disable"
                ? pendingWslChange.wasWslOnly
                  ? "Turn off WSL and switch back to Windows?"
                  : "Disable WSL backend?"
                : pendingWslChange?.kind === "distro"
                  ? "Switch WSL distro?"
                  : pendingWslChange?.kind === "enable"
                    ? "Start the WSL backend"
                    : pendingWslChange?.nextValue
                      ? "Run only the WSL backend?"
                      : "Re-enable the Windows backend?"}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {pendingWslChange?.kind === "disable"
                ? pendingWslChange.wasWslOnly
                  ? "Coddy Studio will restart on the Windows backend. Threads and projects opened against WSL stay safe inside the distro and become available again when you re-enable WSL."
                  : "The WSL backend will stop. Threads and projects opened against WSL stay safe inside the distro, but they'll be unavailable in Coddy Studio until you re-enable WSL."
                : pendingWslChange?.kind === "distro"
                  ? "Coddy Studio will restart the WSL backend on the new distro. Sessions still running on the current distro will be interrupted."
                  : pendingWslChange?.kind === "enable"
                    ? "Run the WSL backend alongside the Windows one, or stop the Windows backend and use only WSL? You can change this later from Settings."
                    : pendingWslChange?.nextValue
                      ? "Coddy Studio will restart and start only the WSL backend. Your Windows-side projects won't be accessible until you turn this off again."
                      : "Coddy Studio will restart and bring the Windows backend back up alongside WSL."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogClose
              disabled={isUpdatingWslBackend}
              render={<Button variant="outline" disabled={isUpdatingWslBackend} />}
            >
              Cancel
            </AlertDialogClose>
            {pendingWslChange?.kind === "enable" ? (
              <>
                <Button
                  variant="outline"
                  onClick={() => handleConfirmEnableWsl("wsl-only")}
                  disabled={isUpdatingWslBackend}
                >
                  {isUpdatingWslBackend ? (
                    <>
                      <Spinner className="size-3.5" />
                      Applying…
                    </>
                  ) : (
                    "Use only WSL"
                  )}
                </Button>
                <Button
                  onClick={() => handleConfirmEnableWsl("both")}
                  disabled={isUpdatingWslBackend}
                >
                  {isUpdatingWslBackend ? (
                    <>
                      <Spinner className="size-3.5" />
                      Applying…
                    </>
                  ) : (
                    "Run both backends"
                  )}
                </Button>
              </>
            ) : (
              <Button
                variant={
                  pendingWslChange?.kind === "disable" ||
                  (pendingWslChange?.kind === "wsl-only" && pendingWslChange.nextValue)
                    ? "destructive"
                    : "default"
                }
                onClick={handleConfirmWslChange}
                disabled={isUpdatingWslBackend}
              >
                {isUpdatingWslBackend ? (
                  <>
                    <Spinner className="size-3.5" />
                    Applying…
                  </>
                ) : pendingWslChange?.kind === "disable" ? (
                  pendingWslChange.wasWslOnly ? (
                    "Switch to Windows"
                  ) : (
                    "Disable WSL"
                  )
                ) : pendingWslChange?.kind === "distro" ? (
                  "Switch distro"
                ) : pendingWslChange?.nextValue ? (
                  "Restart and enable"
                ) : (
                  "Restart and disable"
                )}
              </Button>
            )}
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </SettingsPageContainer>
  );
}
