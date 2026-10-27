import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { EnvironmentId } from "@coddy/contracts";
import type { UnifiedSettings } from "@coddy/contracts/settings";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";

import { desktopLocalBackendId, isDesktopLocalConnectionTarget } from "~/connection/desktopLocal";
import { useDesktopLocalBootstraps } from "~/connection/useDesktopLocalBootstraps";
import { readLocalApi } from "~/localApi";
import { useEnvironmentSettings, useUpdateEnvironmentSettings } from "../../hooks/useSettings";
import {
  type EnvironmentPresentation,
  useEnvironments,
  usePrimaryEnvironment,
} from "../../state/environments";
import { Button } from "../ui/button";
import { DraftInput } from "../ui/draft-input";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { RemoteDirectoryPickerDialog } from "./RemoteDirectoryPickerDialog";
import { SettingResetButton, SettingsRow } from "./settingsLayout";
import {
  canUseNativeHostFolderPicker,
  getEnvironmentBrowsePlatform,
  pickHostFolder,
  readPrimaryRunningDistro,
} from "../hostFolderPicker";

export function WorktreeWorkspaceSetting() {
  const { environments } = useEnvironments();
  const primaryEnvironment = usePrimaryEnvironment();
  const desktopLocalBootstraps = useDesktopLocalBootstraps();
  const initialEnvironmentId =
    primaryEnvironment?.environmentId ?? environments[0]?.environmentId ?? null;
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState<EnvironmentId | null>(
    initialEnvironmentId,
  );

  if (
    selectedEnvironmentId === null ||
    !environments.some((environment) => environment.environmentId === selectedEnvironmentId)
  ) {
    const fallbackEnvironmentId =
      primaryEnvironment?.environmentId ?? environments[0]?.environmentId ?? null;
    if (fallbackEnvironmentId !== selectedEnvironmentId) {
      setSelectedEnvironmentId(fallbackEnvironmentId);
    }
  }

  const selectedEnvironment =
    environments.find((environment) => environment.environmentId === selectedEnvironmentId) ?? null;
  if (selectedEnvironment === null) {
    return (
      <SettingsRow
        title="Workspace"
        description="Connect a host to configure where new worktrees are created."
        control={
          <DraftInput
            value=""
            onCommit={() => undefined}
            disabled
            aria-label="Workspace directory"
          />
        }
      />
    );
  }

  return (
    <EnvironmentWorktreeWorkspaceSetting
      key={selectedEnvironment.environmentId}
      environment={selectedEnvironment}
      environments={environments}
      primaryEnvironmentId={primaryEnvironment?.environmentId ?? null}
      desktopLocalBootstraps={desktopLocalBootstraps}
      onSelectEnvironment={setSelectedEnvironmentId}
    />
  );
}

function EnvironmentWorktreeWorkspaceSetting({
  environment,
  environments,
  primaryEnvironmentId,
  desktopLocalBootstraps,
  onSelectEnvironment,
}: {
  readonly environment: EnvironmentPresentation;
  readonly environments: ReadonlyArray<EnvironmentPresentation>;
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly desktopLocalBootstraps: ReturnType<typeof useDesktopLocalBootstraps>;
  readonly onSelectEnvironment: (environmentId: EnvironmentId) => void;
}) {
  const settings = useEnvironmentSettings(environment.environmentId);
  const latestSettings = useRef(settings);
  useLayoutEffect(() => {
    latestSettings.current = settings;
  }, [settings]);
  const updateSettings = useUpdateEnvironmentSettings(environment.environmentId);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nativeRequest = useRef(0);
  const [confirmedWorkspace, setConfirmedWorkspace] = useState<{
    readonly value: string;
    readonly source: UnifiedSettings;
  } | null>(null);
  const connected = environment.connection.phase === "connected";

  if (confirmedWorkspace !== null && settings !== confirmedWorkspace.source) {
    setConfirmedWorkspace(null);
  }

  useEffect(
    () => () => {
      nativeRequest.current += 1;
    },
    [],
  );

  const save = useCallback(
    async (worktreeBaseDirectory: string) => {
      setPending(true);
      const result = await updateSettings({ worktreeBaseDirectory });
      if (isAtomCommandInterrupted(result)) {
        setPending(false);
        return;
      }
      if (result._tag === "Failure") {
        const failure = squashAtomCommandFailure(result);
        setError(
          failure instanceof Error && failure.message.trim().length > 0
            ? failure.message
            : "Workspace could not be saved.",
        );
      } else {
        setError(null);
        setConfirmedWorkspace({
          value: result.value?.worktreeBaseDirectory ?? worktreeBaseDirectory,
          source: latestSettings.current,
        });
      }
      setPending(false);
    },
    [updateSettings],
  );

  const configured = confirmedWorkspace?.value ?? settings.worktreeBaseDirectory;
  const isPrimary = environment.environmentId === primaryEnvironmentId;
  const desktopInstanceId = isDesktopLocalConnectionTarget(environment.entry.target)
    ? (desktopLocalBootstraps.find((bootstrap) => bootstrap.httpBaseUrl === environment.displayUrl)
        ?.id ?? null)
    : null;
  const nativeTarget = {
    environmentId: environment.environmentId,
    platform: getEnvironmentBrowsePlatform(environment.serverConfig?.environment.platform.os),
    isPrimary,
    desktopInstanceId,
    nativePickerAvailable: typeof window !== "undefined" && window.desktopBridge !== undefined,
  };
  const wslCandidates = environments.flatMap((candidate) => {
    const backendId = desktopLocalBackendId(candidate.entry.target);
    if (backendId === null) return [];
    const bootstrap = desktopLocalBootstraps.find(
      (entry) => entry.httpBaseUrl === candidate.displayUrl,
    );
    return [
      {
        environmentId: candidate.environmentId,
        backendId,
        runningDistro: bootstrap?.runningDistro ?? null,
      },
    ];
  });
  const browse = async () => {
    if (!canUseNativeHostFolderPicker(nativeTarget)) {
      setPickerOpen(true);
      return;
    }
    const api = readLocalApi();
    if (api === undefined) {
      setError("Folder picking is unavailable.");
      return;
    }
    const request = ++nativeRequest.current;
    const requestIsCurrent = () => nativeRequest.current === request;
    try {
      const result = await pickHostFolder({
        host: nativeTarget,
        primaryEnvironmentId,
        initialPath: configured || "~",
        dialogs: api.dialogs,
        getWslState: () =>
          typeof window === "undefined" || window.desktopBridge === undefined
            ? Promise.resolve(null)
            : window.desktopBridge.getWslState(),
        primaryRunningDistro: readPrimaryRunningDistro(),
        wslCandidates,
      });
      switch (result._tag) {
        case "Cancelled":
          return;
        case "Failure":
          if (requestIsCurrent()) setError(result.message);
          return;
        case "Selected":
          if (result.environmentId === environment.environmentId && requestIsCurrent()) {
            await save(result.path);
          }
      }
    } catch (cause) {
      if (requestIsCurrent()) {
        setError(cause instanceof Error ? cause.message : "Folder picking failed.");
      }
    }
  };
  return (
    <>
      <SettingsRow
        title="Workspace"
        description={
          configured
            ? "New worktrees are created inside this host directory."
            : "Default: worktrees are stored next to each project."
        }
        status={
          error ? (
            <span role="alert" className="text-destructive">
              {error}
            </span>
          ) : !connected ? (
            `Reconnect ${environment.label} to change Workspace.`
          ) : null
        }
        resetAction={
          configured && connected && !pending ? (
            <SettingResetButton label="Workspace" onClick={() => void save("")} />
          ) : null
        }
        control={
          <div className="flex w-full flex-wrap items-center justify-end gap-2">
            {environments.length > 1 ? (
              <Select
                value={environment.environmentId}
                disabled={pending}
                onValueChange={(value) => {
                  if (value !== null) onSelectEnvironment(EnvironmentId.make(value));
                }}
              >
                <SelectTrigger aria-label="Workspace host" className="w-full sm:w-40">
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
            ) : null}
            <DraftInput
              value={configured}
              onCommit={(value) => void save(value)}
              disabled={!connected || pending}
              aria-label="Workspace directory"
              spellCheck={false}
              className="w-full font-mono sm:w-72"
            />
            <Button
              type="button"
              variant="outline"
              disabled={!connected || pending}
              onClick={() => void browse()}
            >
              Browse
            </Button>
          </div>
        }
      />
      <RemoteDirectoryPickerDialog
        open={pickerOpen}
        environmentId={environment.environmentId}
        initialPath={configured || "~"}
        onOpenChange={setPickerOpen}
        onSelect={(path) => {
          setPickerOpen(false);
          void save(path);
        }}
      />
    </>
  );
}
