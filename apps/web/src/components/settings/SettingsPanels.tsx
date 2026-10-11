import { ArchiveIcon, ArchiveX, LoaderIcon, PlusIcon, RefreshCwIcon } from "lucide-react";
import { Link, useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAtomValue } from "@effect/atom-react";
import {
  DEFAULT_SERVER_SETTINGS,
  defaultInstanceIdForDriver,
  type EnvironmentId,
  type OrchestrationThreadShell,
  PROVIDER_DISPLAY_NAMES,
  ProviderDriverKind,
  type ProviderInstanceConfig,
  type ProviderInstanceId,
  type ProviderSessionDefault,
  type WorktreeRemovalResult,
} from "@bibcode/contracts";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import { safeErrorLogAttributes } from "@bibcode/client-runtime/errors";
import {
  isAtomCommandInterrupted,
  settlePromise,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import { selectWorktreeCatalogCapabilityPolicy } from "@bibcode/client-runtime/state/worktrees";
import {
  BUNDLED_TERMINAL_FONT_PREFERENCE,
  DEFAULT_DEFAULT_AGENT_SELECTION,
  DEFAULT_UNIFIED_SETTINGS,
  type TerminalFontPreference,
} from "@bibcode/contracts/settings";
import { createModelSelection } from "@bibcode/shared/model";
import * as Arr from "effect/Array";
import * as Equal from "effect/Equal";
import * as Result from "effect/Result";
import { APP_VERSION, HOSTED_APP_CHANNEL, HOSTED_APP_CHANNEL_LABEL } from "../../branding";
import {
  canCheckForUpdate,
  getDesktopUpdateButtonTooltip,
  isDesktopUpdateButtonDisabled,
  resolveDesktopUpdateButtonAction,
} from "../../components/desktopUpdate.logic";
import { UpdateProtectionDialog } from "../desktop/UpdateProtectionDialog";
import { ProviderModelPicker } from "../chat/ProviderModelPicker";
import { ProviderInstanceIcon } from "../chat/ProviderInstanceIcon";
import { TraitsPicker } from "../chat/TraitsPicker";
import {
  buildProviderAgentActions,
  isProviderAgentActionSelectable,
  resolveEffectiveProviderAgentAction,
} from "../chat/providerAgentActions";
import { isDesktopHost } from "../../env";
import { buildHostedChannelSelectionUrl, type HostedAppChannel } from "../../hostedPairing";
import { useTheme } from "../../hooks/useTheme";
import { usePrimarySettings, useUpdatePrimarySettings } from "../../hooks/useSettings";
import { useThreadActions } from "../../hooks/useThreadActions";
import { WorktreeRemovalDialog, type WorktreeRemovalTarget } from "../WorktreeRemovalDialog";
import { useDesktopUpdateState } from "../../state/desktopUpdate";
import {
  getCustomModelOptionsByInstance,
  resolveAppModelSelectionState,
} from "../../modelSelection";
import {
  applyProviderInstanceSettings,
  deriveProviderInstanceEntries,
  sortProviderInstanceEntries,
} from "../../providerInstances";
import { ensureLocalApi, readLocalApi } from "../../localApi";
import { isPreviewSupportedInRuntime } from "../../previewStateStore";
import {
  primaryServerObservabilityAtom,
  primaryServerProvidersAtom,
  serverEnvironment,
} from "../../state/server";
import { usePrimaryEnvironment } from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { useProjects, useServerConfigs } from "../../state/entities";
import { useArchivedThreadSnapshots } from "../../lib/archivedThreadsState";
import {
  isCustomTerminalFontAvailable,
  normalizeCustomTerminalFontFamily,
} from "../../lib/terminalFont";
import { formatRelativeTime, formatRelativeTimeLabel } from "../../timestampFormat";
import { Button } from "../ui/button";
import { Badge } from "../ui/badge";
import { DraftInput } from "../ui/draft-input";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { stackedThreadToast, toastManager } from "../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { AddProviderInstanceDialog } from "./AddProviderInstanceDialog";
import { WorktreeWorkspaceSetting } from "./WorktreeWorkspaceSetting";
import {
  canOneClickUpdateProviderCandidate,
  collectProviderUpdateCandidates,
  hasOneClickUpdateProviderCandidate,
  isProviderUpdateActive,
  type ProviderUpdateCandidate,
} from "../ProviderUpdateLaunchNotification.logic";
import { ProviderInstanceCard } from "./ProviderInstanceCard";
import { DRIVER_OPTIONS, getDriverOption } from "./providerDriverMeta";
import {
  buildProviderInstanceUpdatePatch,
  createSettingsMapDraft,
  createProviderSessionDefaultsDraft,
  formatDiagnosticsDescription,
} from "./SettingsPanels.logic";
import {
  SettingResetButton,
  SettingsPageContainer,
  SettingsRow,
  SettingsSection,
  useRelativeTimeTick,
} from "./settingsLayout";
import { ProjectFavicon } from "../ProjectFavicon";
import { useAtomCommand } from "../../state/use-atom-command";
import { StatusBarSettingsSection } from "./StatusBarSettingsSection";

const THEME_OPTIONS = [
  {
    value: "system",
    label: "System",
  },
  {
    value: "light",
    label: "Light",
  },
  {
    value: "dark",
    label: "Dark",
  },
] as const;

function isPromiseLike(value: unknown): value is PromiseLike<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    "then" in value &&
    typeof value.then === "function"
  );
}

function isSettingsUpdateFailure(result: unknown): boolean {
  return (
    typeof result === "object" && result !== null && "_tag" in result && result._tag === "Failure"
  );
}

const TERMINAL_THEME_OPTIONS = [
  {
    value: "dark",
    label: "Always dark",
  },
  {
    value: "light",
    label: "Always light",
  },
  {
    value: "app",
    label: "Follow app theme",
  },
] as const;

const TERMINAL_FONT_OPTIONS = [
  {
    value: "bundled",
    label: "Bundled Nerd Font",
  },
  {
    value: "system",
    label: "System monospace",
  },
  {
    value: "custom",
    label: "Custom font",
  },
] as const;

const TIMESTAMP_FORMAT_LABELS = {
  locale: "System default",
  "12-hour": "12-hour",
  "24-hour": "24-hour",
} as const;

const BROWSER_LINK_TARGET_LABELS = {
  app: "BiBCode browser",
  system: "System browser",
} as const;

const DEFAULT_DRIVER_KIND = ProviderDriverKind.make("codex");

function terminalFontPreferenceForMode(
  current: TerminalFontPreference,
  mode: unknown,
): TerminalFontPreference | null {
  switch (mode) {
    case "bundled":
      return BUNDLED_TERMINAL_FONT_PREFERENCE;
    case "system":
      return { mode: "system" };
    case "custom":
      return current.mode === "custom" ? current : { mode: "custom", family: "JetBrains Mono" };
    default:
      return null;
  }
}

function terminalFontPreferencesEqual(
  left: TerminalFontPreference,
  right: TerminalFontPreference,
): boolean {
  if (left.mode !== right.mode) return false;
  if (left.mode !== "custom" || right.mode !== "custom") return true;
  return left.family === right.family;
}

function withoutProviderInstanceKey<V>(
  record: Readonly<Record<ProviderInstanceId, V>> | undefined,
  key: ProviderInstanceId,
): Record<ProviderInstanceId, V> {
  const next = { ...record } as Record<ProviderInstanceId, V>;
  delete next[key];
  return next;
}

function withoutProviderInstanceFavorites(
  favorites: ReadonlyArray<{ readonly provider: ProviderInstanceId; readonly model: string }>,
  instanceId: ProviderInstanceId,
) {
  return favorites.filter((favorite) => favorite.provider !== instanceId);
}

const PROVIDER_SETTINGS = DRIVER_OPTIONS.map((definition) => ({
  provider: definition.value,
}));

function ProviderLastChecked({ lastCheckedAt }: { lastCheckedAt: string | null }) {
  useRelativeTimeTick();
  const lastCheckedRelative = lastCheckedAt ? formatRelativeTime(lastCheckedAt) : null;

  if (!lastCheckedRelative) {
    return null;
  }

  return (
    <span className="text-xs text-muted-foreground">
      {lastCheckedRelative.suffix ? (
        <>
          Checked <span className="font-mono tabular-nums">{lastCheckedRelative.value}</span>{" "}
          {lastCheckedRelative.suffix}
        </>
      ) : (
        <>Checked {lastCheckedRelative.value}</>
      )}
    </span>
  );
}

function AboutVersionTitle({ availableVersion }: { readonly availableVersion?: string | null }) {
  return (
    <span className="inline-flex items-center gap-2">
      <span>Version</span>
      <code className="text-xs font-medium text-muted-foreground">
        {APP_VERSION}
        {availableVersion ? <> → {availableVersion}</> : null}
      </code>
    </span>
  );
}

function AboutVersionSection() {
  const navigate = useNavigate();
  const updateState = useDesktopUpdateState();
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);

  const hasDesktopBridge = typeof window !== "undefined" && Boolean(window.desktopBridge);
  const selectedHostedAppChannel = hasDesktopBridge ? null : HOSTED_APP_CHANNEL;

  const handleButtonClick = useCallback(() => {
    const bridge = window.desktopBridge;
    if (!bridge) return;

    const action = updateState ? resolveDesktopUpdateButtonAction(updateState) : "none";

    if (action === "download") {
      void bridge.downloadUpdate().catch((error: unknown) => {
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not download update",
            description: error instanceof Error ? error.message : "Download failed.",
          }),
        );
      });
      return;
    }

    if (action === "install") {
      setUpdateDialogOpen(true);
      return;
    }

    if (typeof bridge.checkForUpdate !== "function") return;
    void bridge
      .checkForUpdate()
      .then((result) => {
        if (
          !result.checked &&
          (result.state.status === "disabled" || result.state.status === "error")
        ) {
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Could not check for updates",
              description:
                result.state.message ?? "Automatic updates are not available in this build.",
            }),
          );
        }
      })
      .catch((error: unknown) => {
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not check for updates",
            description: error instanceof Error ? error.message : "Update check failed.",
          }),
        );
      });
  }, [updateState]);

  const action = updateState ? resolveDesktopUpdateButtonAction(updateState) : "none";
  const buttonTooltip = updateState ? getDesktopUpdateButtonTooltip(updateState) : null;
  const buttonDisabled =
    action === "none"
      ? !canCheckForUpdate(updateState)
      : isDesktopUpdateButtonDisabled(updateState);

  const actionLabel: Record<string, string> = { download: "Download", install: "Install" };
  const statusLabel: Record<string, string> = {
    checking: "Checking…",
    downloading: "Downloading…",
    "up-to-date": "Up to Date",
  };
  const buttonLabel =
    actionLabel[action] ?? statusLabel[updateState?.status ?? ""] ?? "Check for Updates";
  const description =
    action === "download" || action === "install"
      ? "Update available."
      : "Current version of the application.";

  return (
    <>
      <SettingsRow
        title={<AboutVersionTitle availableVersion={updateState?.availableVersion ?? null} />}
        description={description}
        control={
          <Tooltip>
            <TooltipTrigger
              render={
                <Button
                  size="xs"
                  variant={action === "install" ? "default" : "outline"}
                  disabled={buttonDisabled}
                  onClick={handleButtonClick}
                >
                  {buttonLabel}
                </Button>
              }
            />
            {buttonTooltip ? <TooltipPopup>{buttonTooltip}</TooltipPopup> : null}
          </Tooltip>
        }
      />
      {selectedHostedAppChannel ? (
        <SettingsRow
          title="Update track"
          description="Switches the hosted app release channel."
          control={
            <Select
              value={selectedHostedAppChannel}
              onValueChange={(value) => {
                if (value === selectedHostedAppChannel) return;
                window.location.assign(
                  buildHostedChannelSelectionUrl({ channel: value as HostedAppChannel }),
                );
              }}
            >
              <SelectTrigger className="w-full sm:w-40" aria-label="Update track">
                <SelectValue>{HOSTED_APP_CHANNEL_LABEL}</SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                <SelectItem hideIndicator value="latest">
                  Latest
                </SelectItem>
                <SelectItem hideIndicator value="nightly">
                  Nightly
                </SelectItem>
              </SelectPopup>
            </Select>
          }
        />
      ) : null}
      {updateState && window.desktopBridge ? (
        <UpdateProtectionDialog
          open={updateDialogOpen}
          state={updateState}
          onOpenChange={setUpdateDialogOpen}
          installUpdate={(input) => window.desktopBridge!.installUpdate(input)}
          onDiagnostics={() => {
            setUpdateDialogOpen(false);
            void navigate({ to: "/settings/diagnostics" });
          }}
          onError={(description) => {
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Could not install update",
                description,
              }),
            );
          }}
        />
      ) : null}
    </>
  );
}

export function useGeneralSettingsRestore(onRestored?: () => void) {
  const { theme, setTheme } = useTheme();
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();
  // The link target row is hidden where the BiBCode browser can't open links.
  const linkTargetShown = isPreviewSupportedInRuntime();

  const changedSettingLabels = useMemo(
    () => [
      ...(theme !== "system" ? ["Theme"] : []),
      ...(settings.timestampFormat !== DEFAULT_UNIFIED_SETTINGS.timestampFormat
        ? ["Time format"]
        : []),
      ...(linkTargetShown &&
      settings.browserLinkTarget !== DEFAULT_UNIFIED_SETTINGS.browserLinkTarget
        ? ["Open links in"]
        : []),
      ...(settings.wordWrap !== DEFAULT_UNIFIED_SETTINGS.wordWrap ? ["Word wrap"] : []),
      ...(settings.diffIgnoreWhitespace !== DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace
        ? ["Diff whitespace changes"]
        : []),
      ...(settings.enableAssistantStreaming !== DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming
        ? ["Assistant output"]
        : []),
      ...(settings.enableProviderUpdateChecks !==
      DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks
        ? ["Provider update checks"]
        : []),
      ...(settings.autoOpenPlanSidebar !== DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar
        ? ["Auto-open task panel"]
        : []),
      ...(settings.defaultThreadEnvMode !== DEFAULT_UNIFIED_SETTINGS.defaultThreadEnvMode
        ? ["New thread mode"]
        : []),
      ...(settings.newWorktreesStartFromOrigin !==
      DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin
        ? ["New worktrees start from origin"]
        : []),
      ...(settings.worktreeBaseDirectory !== DEFAULT_UNIFIED_SETTINGS.worktreeBaseDirectory
        ? ["Workspace"]
        : []),
      ...(settings.addProjectBaseDirectory !== DEFAULT_UNIFIED_SETTINGS.addProjectBaseDirectory
        ? ["Add project base directory"]
        : []),
      ...(settings.confirmThreadArchive !== DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive
        ? ["Archive confirmation"]
        : []),
      ...(settings.confirmThreadDelete !== DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete
        ? ["Delete confirmation"]
        : []),
    ],
    [
      settings.autoOpenPlanSidebar,
      settings.browserLinkTarget,
      settings.confirmThreadArchive,
      settings.confirmThreadDelete,
      settings.addProjectBaseDirectory,
      settings.defaultThreadEnvMode,
      settings.newWorktreesStartFromOrigin,
      settings.worktreeBaseDirectory,
      settings.diffIgnoreWhitespace,
      settings.enableAssistantStreaming,
      settings.enableProviderUpdateChecks,
      settings.timestampFormat,
      settings.wordWrap,
      linkTargetShown,
      theme,
    ],
  );

  const restoreDefaults = useCallback(async () => {
    if (changedSettingLabels.length === 0) return;
    const api = readLocalApi();
    const confirmed = await (api ?? ensureLocalApi()).dialogs.confirm(
      ["Restore General defaults?", `This will reset: ${changedSettingLabels.join(", ")}.`].join(
        "\n",
      ),
    );
    if (!confirmed) return;

    setTheme("system");
    updateSettings({
      timestampFormat: DEFAULT_UNIFIED_SETTINGS.timestampFormat,
      ...(linkTargetShown ? { browserLinkTarget: DEFAULT_UNIFIED_SETTINGS.browserLinkTarget } : {}),
      wordWrap: DEFAULT_UNIFIED_SETTINGS.wordWrap,
      diffIgnoreWhitespace: DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace,
      enableAssistantStreaming: DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming,
      enableProviderUpdateChecks: DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks,
      autoOpenPlanSidebar: DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar,
      defaultThreadEnvMode: DEFAULT_UNIFIED_SETTINGS.defaultThreadEnvMode,
      newWorktreesStartFromOrigin: DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin,
      worktreeBaseDirectory: DEFAULT_UNIFIED_SETTINGS.worktreeBaseDirectory,
      addProjectBaseDirectory: DEFAULT_UNIFIED_SETTINGS.addProjectBaseDirectory,
      confirmThreadArchive: DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive,
      confirmThreadDelete: DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete,
    });
    onRestored?.();
  }, [changedSettingLabels, linkTargetShown, onRestored, setTheme, updateSettings]);

  return {
    changedSettingLabels,
    restoreDefaults,
  };
}

export function GeneralSettingsPanel() {
  const { theme, setTheme } = useTheme();
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();
  const linkTargetShown = isPreviewSupportedInRuntime();

  return (
    <SettingsPageContainer>
      <SettingsSection title="General">
        <SettingsRow
          title="Theme"
          description="Choose how BiBCode looks across the app."
          resetAction={
            theme !== "system" ? (
              <SettingResetButton label="theme" onClick={() => setTheme("system")} />
            ) : null
          }
          control={
            <Select
              value={theme}
              onValueChange={(value) => {
                if (value === "system" || value === "light" || value === "dark") {
                  setTheme(value);
                }
              }}
            >
              <SelectTrigger className="w-full sm:w-40" aria-label="Theme preference">
                <SelectValue>
                  {THEME_OPTIONS.find((option) => option.value === theme)?.label ?? "System"}
                </SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                {THEME_OPTIONS.map((option) => (
                  <SelectItem hideIndicator key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          }
        />

        <SettingsRow
          title="Time format"
          description="System default follows your browser or OS clock preference."
          resetAction={
            settings.timestampFormat !== DEFAULT_UNIFIED_SETTINGS.timestampFormat ? (
              <SettingResetButton
                label="time format"
                onClick={() =>
                  updateSettings({
                    timestampFormat: DEFAULT_UNIFIED_SETTINGS.timestampFormat,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={settings.timestampFormat}
              onValueChange={(value) => {
                if (value === "locale" || value === "12-hour" || value === "24-hour") {
                  updateSettings({ timestampFormat: value });
                }
              }}
            >
              <SelectTrigger className="w-full sm:w-40" aria-label="Timestamp format">
                <SelectValue>{TIMESTAMP_FORMAT_LABELS[settings.timestampFormat]}</SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                <SelectItem hideIndicator value="locale">
                  {TIMESTAMP_FORMAT_LABELS.locale}
                </SelectItem>
                <SelectItem hideIndicator value="12-hour">
                  {TIMESTAMP_FORMAT_LABELS["12-hour"]}
                </SelectItem>
                <SelectItem hideIndicator value="24-hour">
                  {TIMESTAMP_FORMAT_LABELS["24-hour"]}
                </SelectItem>
              </SelectPopup>
            </Select>
          }
        />

        {linkTargetShown ? (
          <SettingsRow
            title="Open links in"
            description="Where web links from chat and the terminal open. Modifier-click a chat link, or Ctrl/Cmd+Shift-click a terminal link, to use the other one."
            resetAction={
              settings.browserLinkTarget !== DEFAULT_UNIFIED_SETTINGS.browserLinkTarget ? (
                <SettingResetButton
                  label="link target"
                  onClick={() =>
                    updateSettings({
                      browserLinkTarget: DEFAULT_UNIFIED_SETTINGS.browserLinkTarget,
                    })
                  }
                />
              ) : null
            }
            control={
              <Select
                value={settings.browserLinkTarget}
                onValueChange={(value) => {
                  if (value === "app" || value === "system") {
                    updateSettings({ browserLinkTarget: value });
                  }
                }}
              >
                <SelectTrigger className="w-full sm:w-40" aria-label="Open links in">
                  <SelectValue>
                    {BROWSER_LINK_TARGET_LABELS[settings.browserLinkTarget]}
                  </SelectValue>
                </SelectTrigger>
                <SelectPopup align="end" alignItemWithTrigger={false}>
                  <SelectItem hideIndicator value="app">
                    {BROWSER_LINK_TARGET_LABELS.app}
                  </SelectItem>
                  <SelectItem hideIndicator value="system">
                    {BROWSER_LINK_TARGET_LABELS.system}
                  </SelectItem>
                </SelectPopup>
              </Select>
            }
          />
        ) : null}

        <SettingsRow
          title="Word wrap"
          description="Wrap long lines in code blocks, tables, diffs, and file previews by default."
          resetAction={
            settings.wordWrap !== DEFAULT_UNIFIED_SETTINGS.wordWrap ? (
              <SettingResetButton
                label="word wrapping"
                onClick={() =>
                  updateSettings({
                    wordWrap: DEFAULT_UNIFIED_SETTINGS.wordWrap,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.wordWrap}
              onCheckedChange={(checked) => updateSettings({ wordWrap: Boolean(checked) })}
              aria-label="Wrap code, tables, diffs, and file previews by default"
            />
          }
        />

        <SettingsRow
          title="Hide whitespace changes"
          description="Set whether the diff panel ignores whitespace-only edits by default."
          resetAction={
            settings.diffIgnoreWhitespace !== DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace ? (
              <SettingResetButton
                label="diff whitespace changes"
                onClick={() =>
                  updateSettings({
                    diffIgnoreWhitespace: DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.diffIgnoreWhitespace}
              onCheckedChange={(checked) =>
                updateSettings({ diffIgnoreWhitespace: Boolean(checked) })
              }
              aria-label="Hide whitespace changes by default"
            />
          }
        />

        <SettingsRow
          title="Assistant output"
          description="Show token-by-token output while a response is in progress."
          resetAction={
            settings.enableAssistantStreaming !==
            DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming ? (
              <SettingResetButton
                label="assistant output"
                onClick={() =>
                  updateSettings({
                    enableAssistantStreaming: DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.enableAssistantStreaming}
              onCheckedChange={(checked) =>
                updateSettings({ enableAssistantStreaming: Boolean(checked) })
              }
              aria-label="Stream assistant messages"
            />
          }
        />

        <SettingsRow
          title="Provider update checks"
          description="Check installed provider CLIs for newer available versions."
          resetAction={
            settings.enableProviderUpdateChecks !==
            DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks ? (
              <SettingResetButton
                label="provider update checks"
                onClick={() =>
                  updateSettings({
                    enableProviderUpdateChecks: DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.enableProviderUpdateChecks}
              onCheckedChange={(checked) =>
                updateSettings({ enableProviderUpdateChecks: Boolean(checked) })
              }
              aria-label="Check provider versions"
            />
          }
        />

        <SettingsRow
          title="Auto-open task panel"
          description="Open the right-side plan and task panel automatically when steps appear."
          resetAction={
            settings.autoOpenPlanSidebar !== DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar ? (
              <SettingResetButton
                label="auto-open task panel"
                onClick={() =>
                  updateSettings({
                    autoOpenPlanSidebar: DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.autoOpenPlanSidebar}
              onCheckedChange={(checked) =>
                updateSettings({ autoOpenPlanSidebar: Boolean(checked) })
              }
              aria-label="Open the task panel automatically"
            />
          }
        />

        <SettingsRow
          title="New threads"
          description="Pick the default workspace mode for newly created draft threads."
          resetAction={
            settings.defaultThreadEnvMode !== DEFAULT_UNIFIED_SETTINGS.defaultThreadEnvMode ||
            settings.newWorktreesStartFromOrigin !==
              DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin ? (
              <SettingResetButton
                label="new threads"
                onClick={() =>
                  updateSettings({
                    defaultThreadEnvMode: DEFAULT_UNIFIED_SETTINGS.defaultThreadEnvMode,
                    newWorktreesStartFromOrigin:
                      DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={settings.defaultThreadEnvMode}
              onValueChange={(value) => {
                if (value === "local" || value === "worktree") {
                  updateSettings({ defaultThreadEnvMode: value });
                }
              }}
            >
              <SelectTrigger className="w-full sm:w-44" aria-label="Default thread mode">
                <SelectValue>
                  {settings.defaultThreadEnvMode === "worktree" ? "New worktree" : "Local"}
                </SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                <SelectItem hideIndicator value="local">
                  Local
                </SelectItem>
                <SelectItem hideIndicator value="worktree">
                  New worktree
                </SelectItem>
              </SelectPopup>
            </Select>
          }
        />

        {settings.defaultThreadEnvMode === "worktree" ? (
          <SettingsRow
            className="bg-muted/20 sm:pl-9"
            title="Start from origin"
            description="Creates the worktree from the latest matching branch on origin instead of your local branch."
            resetAction={
              settings.newWorktreesStartFromOrigin !==
              DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin ? (
                <SettingResetButton
                  label="new worktrees start from origin"
                  onClick={() =>
                    updateSettings({
                      newWorktreesStartFromOrigin:
                        DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin,
                    })
                  }
                />
              ) : null
            }
            control={
              <Switch
                checked={settings.newWorktreesStartFromOrigin}
                onCheckedChange={(checked) =>
                  updateSettings({ newWorktreesStartFromOrigin: Boolean(checked) })
                }
                aria-label="Start new worktrees from origin by default"
              />
            }
          />
        ) : null}

        <WorktreeWorkspaceSetting />

        <SettingsRow
          title="Add project starts in"
          description='Leave empty to use "~/" for Clone and Create in Add Project.'
          resetAction={
            settings.addProjectBaseDirectory !==
            DEFAULT_UNIFIED_SETTINGS.addProjectBaseDirectory ? (
              <SettingResetButton
                label="add project base directory"
                onClick={() =>
                  updateSettings({
                    addProjectBaseDirectory: DEFAULT_UNIFIED_SETTINGS.addProjectBaseDirectory,
                  })
                }
              />
            ) : null
          }
          control={
            <DraftInput
              className="w-full sm:w-72"
              value={settings.addProjectBaseDirectory}
              onCommit={(next) => updateSettings({ addProjectBaseDirectory: next })}
              placeholder="~/"
              spellCheck={false}
              aria-label="Add project base directory"
            />
          }
        />

        <SettingsRow
          title="Archive confirmation"
          description="Require a second click on the inline archive action before a thread is archived."
          resetAction={
            settings.confirmThreadArchive !== DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive ? (
              <SettingResetButton
                label="archive confirmation"
                onClick={() =>
                  updateSettings({
                    confirmThreadArchive: DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.confirmThreadArchive}
              onCheckedChange={(checked) =>
                updateSettings({ confirmThreadArchive: Boolean(checked) })
              }
              aria-label="Confirm thread archiving"
            />
          }
        />

        <SettingsRow
          title="Delete confirmation"
          description="Ask before deleting a thread and its chat history."
          resetAction={
            settings.confirmThreadDelete !== DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete ? (
              <SettingResetButton
                label="delete confirmation"
                onClick={() =>
                  updateSettings({
                    confirmThreadDelete: DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.confirmThreadDelete}
              onCheckedChange={(checked) =>
                updateSettings({ confirmThreadDelete: Boolean(checked) })
              }
              aria-label="Confirm thread deletion"
            />
          }
        />
      </SettingsSection>
    </SettingsPageContainer>
  );
}

export function AgentsSettingsPanel() {
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();
  const serverProviders = useAtomValue(primaryServerProvidersAtom);
  const defaultAgentActions = buildProviderAgentActions(serverProviders, settings);
  const selectableDefaultAgentActions = defaultAgentActions.filter(isProviderAgentActionSelectable);
  const effectiveDefaultAgent = resolveEffectiveProviderAgentAction(
    defaultAgentActions,
    settings.defaultAgent,
  );
  const isDefaultAgentDirty = !Equal.equals(settings.defaultAgent, DEFAULT_DEFAULT_AGENT_SELECTION);
  const textGenerationModelSelection = resolveAppModelSelectionState(settings, serverProviders);
  const textGenInstanceId = textGenerationModelSelection.instanceId;
  const textGenModel = textGenerationModelSelection.model;
  const textGenModelOptions = textGenerationModelSelection.options;
  const gitModelInstanceEntries = sortProviderInstanceEntries(
    applyProviderInstanceSettings(deriveProviderInstanceEntries(serverProviders), settings),
  );
  const textGenInstanceEntry = gitModelInstanceEntries.find(
    (entry) => entry.instanceId === textGenInstanceId,
  );
  const textGenProvider: ProviderDriverKind =
    textGenInstanceEntry?.driverKind ?? DEFAULT_DRIVER_KIND;
  const gitModelOptionsByInstance = getCustomModelOptionsByInstance(
    settings,
    serverProviders,
    textGenInstanceId,
    textGenModel,
  );
  const isGitWritingModelDirty = !Equal.equals(
    settings.textGenerationModelSelection ?? null,
    DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection ?? null,
  );

  return (
    <SettingsPageContainer>
      <SettingsSection title="Agents">
        <SettingsRow
          title="Default Agent"
          description="Choose the Main chat or AI terminal opened for newly added projects and worktrees."
          resetAction={
            isDefaultAgentDirty ? (
              <SettingResetButton
                label="default agent"
                onClick={() => updateSettings({ defaultAgent: DEFAULT_DEFAULT_AGENT_SELECTION })}
              />
            ) : null
          }
          control={
            selectableDefaultAgentActions.length === 0 ? (
              <span className="text-sm text-muted-foreground">No enabled agents available</span>
            ) : (
              <Select
                value={effectiveDefaultAgent?.value}
                onValueChange={(value) => {
                  const action = selectableDefaultAgentActions.find((item) => item.value === value);
                  if (action) updateSettings({ defaultAgent: action.selection });
                }}
                items={selectableDefaultAgentActions.map((action) => ({
                  value: action.value,
                  label: action.label,
                }))}
              >
                <SelectTrigger aria-label="Default Agent">
                  <SelectValue />
                </SelectTrigger>
                <SelectPopup>
                  {selectableDefaultAgentActions.map((action) => (
                    <SelectItem key={action.value} value={action.value}>
                      <ProviderInstanceIcon
                        driverKind={action.entry.driverKind}
                        displayName={action.entry.displayName}
                        accentColor={action.entry.accentColor}
                        iconClassName="size-4"
                      />
                      {action.label}
                    </SelectItem>
                  ))}
                </SelectPopup>
              </Select>
            )
          }
        />
        <SettingsRow
          title="Chat agent activity"
          titleTag={
            <Badge variant="warning" size="sm">
              Experimental
            </Badge>
          }
          description="Show live agent and background-task activity in the Chat panel. Disabling this stops Chat activity monitoring and collection."
          resetAction={
            settings.enableChatAgentActivity !== DEFAULT_SERVER_SETTINGS.enableChatAgentActivity ? (
              <SettingResetButton
                label="chat agent activity"
                onClick={() =>
                  updateSettings({
                    enableChatAgentActivity: DEFAULT_SERVER_SETTINGS.enableChatAgentActivity,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.enableChatAgentActivity}
              onCheckedChange={(checked) =>
                updateSettings({ enableChatAgentActivity: Boolean(checked) })
              }
              aria-label="Chat agent activity"
            />
          }
        />
        <SettingsRow
          title="AI Terminal agent activity"
          titleTag={
            <Badge variant="warning" size="sm">
              Experimental
            </Badge>
          }
          description="Show live agent and background-task activity in AI Terminals. Disabling this stops AI Terminal activity monitoring and collection."
          resetAction={
            settings.enableTerminalAgentActivity !==
            DEFAULT_SERVER_SETTINGS.enableTerminalAgentActivity ? (
              <SettingResetButton
                label="AI Terminal agent activity"
                onClick={() =>
                  updateSettings({
                    enableTerminalAgentActivity:
                      DEFAULT_SERVER_SETTINGS.enableTerminalAgentActivity,
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.enableTerminalAgentActivity}
              onCheckedChange={(checked) =>
                updateSettings({ enableTerminalAgentActivity: Boolean(checked) })
              }
              aria-label="AI Terminal agent activity"
            />
          }
        />
        <SettingsRow
          title="Text generation model"
          description="Configure the model used for generated commit messages, PR titles, and similar Git text."
          resetAction={
            isGitWritingModelDirty ? (
              <SettingResetButton
                label="text generation model"
                onClick={() =>
                  updateSettings({
                    textGenerationModelSelection:
                      DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection,
                  })
                }
              />
            ) : null
          }
          control={
            <div className="flex flex-wrap items-center justify-end gap-1.5">
              <ProviderModelPicker
                activeInstanceId={textGenInstanceId}
                model={textGenModel}
                lockedProvider={null}
                instanceEntries={gitModelInstanceEntries}
                modelOptionsByInstance={gitModelOptionsByInstance}
                triggerVariant="outline"
                triggerClassName="min-w-0 max-w-none shrink-0 text-foreground/90 hover:text-foreground"
                onInstanceModelChange={(instanceId, model) => {
                  updateSettings({
                    textGenerationModelSelection: resolveAppModelSelectionState(
                      {
                        ...settings,
                        textGenerationModelSelection: createModelSelection(instanceId, model),
                      },
                      serverProviders,
                    ),
                  });
                }}
              />
              <TraitsPicker
                provider={textGenProvider}
                models={
                  // Use the exact instance's models (rather than the
                  // first-kind-match) so a custom text-gen instance like
                  // `codex_personal` gets its own model list, not the
                  // default Codex one.
                  textGenInstanceEntry?.models ?? []
                }
                model={textGenModel}
                prompt=""
                onPromptChange={() => {}}
                modelOptions={textGenModelOptions}
                allowPromptInjectedEffort={false}
                triggerVariant="outline"
                triggerClassName="min-w-0 max-w-none shrink-0 text-foreground/90 hover:text-foreground"
                onModelOptionsChange={(nextOptions) => {
                  updateSettings({
                    textGenerationModelSelection: resolveAppModelSelectionState(
                      {
                        ...settings,
                        textGenerationModelSelection: createModelSelection(
                          textGenInstanceId,
                          textGenModel,
                          nextOptions,
                        ),
                      },
                      serverProviders,
                    ),
                  });
                }}
              />
            </div>
          }
        />
      </SettingsSection>
    </SettingsPageContainer>
  );
}

export function StatusBarSettingsPanel() {
  return (
    <SettingsPageContainer>
      <StatusBarSettingsSection />
    </SettingsPageContainer>
  );
}

export function TerminalSettingsPanel() {
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();
  const terminalFontPreference = settings.terminalFontPreference;
  const customTerminalFontAvailable =
    terminalFontPreference.mode === "custom"
      ? isCustomTerminalFontAvailable(terminalFontPreference.family)
      : null;

  return (
    <SettingsPageContainer>
      <SettingsSection title="Terminal">
        <SettingsRow
          title="Terminal theme"
          description="Agent TUIs such as Codex paint their own dark panels and never ask the terminal which colours it uses, so a light terminal can show those panels as dark blocks. Choose Always dark if you run them. This preference is stored only on this device."
          resetAction={
            settings.terminalThemePreference !==
            DEFAULT_UNIFIED_SETTINGS.terminalThemePreference ? (
              <SettingResetButton
                label="terminal theme"
                onClick={() =>
                  updateSettings({
                    terminalThemePreference: DEFAULT_UNIFIED_SETTINGS.terminalThemePreference,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={settings.terminalThemePreference}
              onValueChange={(value) => {
                const option = TERMINAL_THEME_OPTIONS.find((entry) => entry.value === value);
                if (option) {
                  updateSettings({ terminalThemePreference: option.value });
                }
              }}
            >
              <SelectTrigger className="w-full sm:w-48" aria-label="Terminal theme">
                <SelectValue>
                  {TERMINAL_THEME_OPTIONS.find(
                    (option) => option.value === settings.terminalThemePreference,
                  )?.label ?? "Follow app theme"}
                </SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                {TERMINAL_THEME_OPTIONS.map((option) => (
                  <SelectItem hideIndicator key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          }
        />
        <SettingsRow
          title="Terminal font"
          description="The bundled monospaced Nerd Font keeps prompt icons aligned with the cursor. This preference is stored only on this device."
          resetAction={
            !terminalFontPreferencesEqual(
              terminalFontPreference,
              DEFAULT_UNIFIED_SETTINGS.terminalFontPreference,
            ) ? (
              <SettingResetButton
                label="terminal font"
                onClick={() =>
                  updateSettings({
                    terminalFontPreference: DEFAULT_UNIFIED_SETTINGS.terminalFontPreference,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={terminalFontPreference.mode}
              onValueChange={(mode) => {
                const nextPreference = terminalFontPreferenceForMode(terminalFontPreference, mode);
                if (nextPreference !== null) {
                  updateSettings({ terminalFontPreference: nextPreference });
                }
              }}
            >
              <SelectTrigger className="w-full sm:w-48" aria-label="Terminal font">
                <SelectValue>
                  {TERMINAL_FONT_OPTIONS.find(
                    (option) => option.value === terminalFontPreference.mode,
                  )?.label ?? "Bundled Nerd Font"}
                </SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                {TERMINAL_FONT_OPTIONS.map((option) => (
                  <SelectItem hideIndicator key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          }
        />
        {terminalFontPreference.mode === "custom" ? (
          <SettingsRow
            title="Custom font family"
            description="Enter one installed font family name. Use a monospaced Nerd Font when your prompt includes icons."
            status={
              customTerminalFontAvailable === false
                ? "This font is not available on this device."
                : null
            }
            control={
              <DraftInput
                value={terminalFontPreference.family}
                aria-label="Custom terminal font family"
                onCommit={(input) => {
                  const family = normalizeCustomTerminalFontFamily(input);
                  if (family !== null) {
                    updateSettings({
                      terminalFontPreference: { mode: "custom", family },
                    });
                  }
                }}
              />
            }
          />
        ) : null}
        <SettingsRow
          title="WebGL renderer"
          description="Render terminals with the GPU-accelerated WebGL renderer. Falls back automatically if WebGL is unavailable."
          resetAction={
            settings.terminal.webglEnabled !== DEFAULT_UNIFIED_SETTINGS.terminal.webglEnabled ? (
              <SettingResetButton
                label="WebGL renderer"
                onClick={() =>
                  updateSettings({
                    terminal: {
                      webglEnabled: DEFAULT_UNIFIED_SETTINGS.terminal.webglEnabled,
                    },
                  })
                }
              />
            ) : null
          }
          control={
            <Switch
              checked={settings.terminal.webglEnabled}
              onCheckedChange={(checked) =>
                updateSettings({ terminal: { webglEnabled: Boolean(checked) } })
              }
              aria-label="Use WebGL terminal renderer"
            />
          }
        />
      </SettingsSection>
    </SettingsPageContainer>
  );
}

export function AboutSettingsPanel() {
  const observability = useAtomValue(primaryServerObservabilityAtom);
  const diagnosticsDescription = formatDiagnosticsDescription({
    localTracingEnabled: observability?.localTracingEnabled ?? false,
  });

  return (
    <SettingsPageContainer>
      <SettingsSection title="About">
        {isDesktopHost || HOSTED_APP_CHANNEL ? (
          <AboutVersionSection />
        ) : (
          <SettingsRow
            title={<AboutVersionTitle />}
            description="Current version of the application."
          />
        )}
        <SettingsRow
          title="Diagnostics"
          description={diagnosticsDescription}
          control={
            <Button render={<Link to="/settings/diagnostics" />} size="xs" variant="outline">
              View diagnostics
            </Button>
          }
        />
      </SettingsSection>
    </SettingsPageContainer>
  );
}

export function ProviderSettingsPanel() {
  const primaryEnvironment = usePrimaryEnvironment();
  return (
    <EnvironmentScopedProviderSettingsPanel
      key={primaryEnvironment?.environmentId ?? "disconnected"}
      primaryEnvironment={primaryEnvironment}
    />
  );
}

function EnvironmentScopedProviderSettingsPanel({
  primaryEnvironment,
}: {
  readonly primaryEnvironment: ReturnType<typeof usePrimaryEnvironment>;
}) {
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();
  const serverProviders = useAtomValue(primaryServerProvidersAtom);
  const refreshServerProviders = useAtomCommand(serverEnvironment.refreshProviders, {
    reportFailure: false,
  });
  const refreshProviderUsage = useAtomCommand(serverEnvironment.refreshProviderUsage, {
    reportFailure: false,
  });
  const providerUsage = useEnvironmentQuery(
    primaryEnvironment
      ? serverEnvironment.providerUsage({
          environmentId: primaryEnvironment.environmentId,
          input: {},
        })
      : null,
  );
  const updateProvider = useAtomCommand(serverEnvironment.updateProvider, {
    reportFailure: false,
  });
  const [isRefreshingProviders, setIsRefreshingProviders] = useState(false);
  const [isAddInstanceDialogOpen, setIsAddInstanceDialogOpen] = useState(false);
  const [updatingProviderDrivers, setUpdatingProviderDrivers] = useState<
    ReadonlySet<ProviderDriverKind>
  >(() => new Set());
  const [openInstanceDetails, setOpenInstanceDetails] = useState<Record<string, boolean>>({});
  const refreshingRef = useRef(false);
  const sessionDefaultsDraftRef = useRef(
    createProviderSessionDefaultsDraft(settings.providerSessionDefaults),
  );
  const providerInstancesDraftRef = useRef(
    createSettingsMapDraft(
      settings.providerInstances ?? {},
      (instance: ProviderInstanceConfig) => ({ ...instance }),
      Equal.equals,
    ),
  );
  const [providerSessionDefaults, setProviderSessionDefaults] = useState(
    settings.providerSessionDefaults,
  );
  const [providerInstances, setProviderInstances] = useState(settings.providerInstances ?? {});

  useEffect(() => {
    const reconciled = sessionDefaultsDraftRef.current.reconcile(settings.providerSessionDefaults);
    setProviderSessionDefaults((current) =>
      Equal.equals(current, reconciled) ? current : reconciled,
    );
  }, [settings.providerSessionDefaults]);

  useEffect(() => {
    const reconciled = providerInstancesDraftRef.current.reconcile(
      settings.providerInstances ?? {},
    );
    setProviderInstances((current) => (Equal.equals(current, reconciled) ? current : reconciled));
  }, [settings.providerInstances]);

  const providerUpdateCandidates = useMemo(
    () => collectProviderUpdateCandidates(serverProviders),
    [serverProviders],
  );
  const providerUpdateCandidateByInstanceId = useMemo(
    () => new Map(providerUpdateCandidates.map((candidate) => [candidate.instanceId, candidate])),
    [providerUpdateCandidates],
  );
  const visibleProviderSettings = PROVIDER_SETTINGS.filter(
    (providerSettings) => providerSettings.provider !== "grok",
  );
  const textGenerationModelSelection = resolveAppModelSelectionState(settings, serverProviders);
  const textGenInstanceId = textGenerationModelSelection.instanceId;
  const lastCheckedAt =
    serverProviders.length > 0
      ? serverProviders.reduce(
          (latest, provider) => (provider.checkedAt > latest ? provider.checkedAt : latest),
          serverProviders[0]!.checkedAt,
        )
      : null;

  const refreshProviders = useCallback(() => {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    setIsRefreshingProviders(true);
    if (!primaryEnvironment) {
      refreshingRef.current = false;
      setIsRefreshingProviders(false);
      return;
    }
    void (async () => {
      const result = await refreshServerProviders({
        environmentId: primaryEnvironment.environmentId,
        input: {},
      });
      refreshingRef.current = false;
      setIsRefreshingProviders(false);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        console.warn("Failed to refresh providers", {
          operation: "refresh-providers",
          environmentId: primaryEnvironment.environmentId,
          ...safeErrorLogAttributes(squashAtomCommandFailure(result)),
        });
      }
    })();
  }, [primaryEnvironment, refreshServerProviders]);

  const refreshClaudeUsage = useCallback(async () => {
    if (!primaryEnvironment) return;
    try {
      await refreshProviderUsage({
        environmentId: primaryEnvironment.environmentId,
        input: { providers: ["claude"], force: true },
      });
    } catch {
      // Command failures are represented by the command layer. Always re-read
      // the query so the status bar observes any snapshot the server committed.
    } finally {
      providerUsage.refresh();
    }
  }, [primaryEnvironment, providerUsage.refresh, refreshProviderUsage]);

  const runProviderUpdate = useCallback(
    async (candidate: ProviderUpdateCandidate) => {
      if (!primaryEnvironment) return;
      let started = false;
      setUpdatingProviderDrivers((previous) => {
        if (previous.has(candidate.driver)) {
          return previous;
        }
        started = true;
        const next = new Set(previous);
        next.add(candidate.driver);
        return next;
      });
      if (!started) {
        return;
      }

      const result = await updateProvider({
        environmentId: primaryEnvironment.environmentId,
        input: {
          provider: candidate.driver,
          instanceId: candidate.instanceId,
        },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: `Could not update ${PROVIDER_DISPLAY_NAMES[candidate.driver] ?? candidate.driver}`,
            description:
              error instanceof Error
                ? error.message
                : "The provider update command could not be started.",
          }),
        );
      }
      setUpdatingProviderDrivers((previous) => {
        if (!previous.has(candidate.driver)) {
          return previous;
        }
        const next = new Set(previous);
        next.delete(candidate.driver);
        return next;
      });
    },
    [primaryEnvironment, updateProvider],
  );

  interface InstanceRow {
    readonly instanceId: ProviderInstanceId;
    readonly instance: ProviderInstanceConfig;
    readonly driver: ProviderDriverKind;
    readonly isDefault: boolean;
    readonly isDirty?: boolean;
  }

  const instancesByDriver = new Map<
    ProviderDriverKind,
    Array<[ProviderInstanceId, ProviderInstanceConfig]>
  >();
  for (const [rawId, instance] of Object.entries(providerInstances)) {
    const driver = instance.driver;
    if (driver === "grok") continue;
    const list = instancesByDriver.get(driver) ?? [];
    list.push([rawId as ProviderInstanceId, instance]);
    instancesByDriver.set(driver, list);
  }

  const defaultSlotIdsBySource = new Set<string>(
    visibleProviderSettings.map((providerSettings) =>
      String(defaultInstanceIdForDriver(providerSettings.provider)),
    ),
  );

  const rows: InstanceRow[] = [];
  const visibleDriverKinds = new Set<ProviderDriverKind>(
    visibleProviderSettings.map((providerSettings) => providerSettings.provider),
  );

  for (const providerSettings of visibleProviderSettings) {
    type LegacyProviderSettings = (typeof settings.providers)[keyof typeof settings.providers];
    const legacyProviders = settings.providers as Record<string, LegacyProviderSettings>;
    const defaultLegacyProviders = DEFAULT_UNIFIED_SETTINGS.providers as Record<
      string,
      LegacyProviderSettings
    >;
    const driver = providerSettings.provider;
    const defaultInstanceId = defaultInstanceIdForDriver(driver);
    const explicitInstance = providerInstances[defaultInstanceId];
    const legacyConfig = legacyProviders[providerSettings.provider]!;
    const defaultLegacyConfig = defaultLegacyProviders[providerSettings.provider]!;
    const effectiveInstance: ProviderInstanceConfig =
      explicitInstance ??
      ({
        driver,
        enabled: legacyConfig.enabled,
        config: legacyConfig,
      } satisfies ProviderInstanceConfig);
    const isDirty =
      explicitInstance !== undefined || !Equal.equals(legacyConfig, defaultLegacyConfig);
    rows.push({
      instanceId: defaultInstanceId,
      instance: effectiveInstance,
      driver,
      isDefault: true,
      isDirty,
    });
    for (const [id, instance] of instancesByDriver.get(providerSettings.provider) ?? []) {
      if (id === defaultInstanceId) continue;
      rows.push({ instanceId: id, instance, driver: instance.driver, isDefault: false });
    }
  }
  for (const [driver, list] of instancesByDriver) {
    if (visibleDriverKinds.has(driver)) continue;
    for (const [id, instance] of list) {
      rows.push({
        instanceId: id,
        instance,
        driver: instance.driver,
        isDefault: defaultSlotIdsBySource.has(String(id)),
      });
    }
  }

  const updateProviderInstance = (
    row: InstanceRow,
    next: ProviderInstanceConfig,
    options?: {
      readonly textGenerationModelSelection?: Parameters<
        typeof buildProviderInstanceUpdatePatch
      >[0]["textGenerationModelSelection"];
      readonly onSuccess?: () => void | Promise<void>;
    },
  ) => {
    const submission = providerInstancesDraftRef.current.submit(row.instanceId, next);
    setProviderInstances(submission.map);
    const updateResult: unknown = updateSettings(
      buildProviderInstanceUpdatePatch({
        settings: {
          ...settings,
          providerInstances: submission.map,
        },
        instanceId: row.instanceId,
        instance: next,
        driver: row.driver,
        isDefault: row.isDefault,
        textGenerationModelSelection: options?.textGenerationModelSelection,
      }),
    );
    if (!isPromiseLike(updateResult)) return;

    const applyRejectedSubmission = () => {
      const rejected = providerInstancesDraftRef.current.reject(submission.revision);
      setProviderInstances((current) => (Equal.equals(current, rejected) ? current : rejected));
    };
    void Promise.resolve(updateResult).then((result) => {
      if (isSettingsUpdateFailure(result)) {
        applyRejectedSubmission();
        return;
      }
      void options?.onSuccess?.();
    }, applyRejectedSubmission);
  };

  const deleteProviderInstance = (id: ProviderInstanceId) => {
    updateSettings({
      providerInstances: withoutProviderInstanceKey(providerInstances, id),
      providerModelPreferences: withoutProviderInstanceKey(settings.providerModelPreferences, id),
      favorites: withoutProviderInstanceFavorites(settings.favorites ?? [], id),
    });
  };

  const updateSessionDefaults = (driver: ProviderDriverKind, next: ProviderSessionDefault) => {
    const submission = sessionDefaultsDraftRef.current.submit(driver, next);
    setProviderSessionDefaults(submission.defaults);
    const updateResult: unknown = updateSettings({
      providerSessionDefaults: submission.defaults,
    });
    if (!isPromiseLike(updateResult)) return;

    const applyRejectedSubmission = () => {
      const rejected = sessionDefaultsDraftRef.current.reject(submission.revision);
      setProviderSessionDefaults((current) =>
        Equal.equals(current, rejected) ? current : rejected,
      );
    };
    void Promise.resolve(updateResult).then((result) => {
      if (isSettingsUpdateFailure(result)) {
        applyRejectedSubmission();
      }
    }, applyRejectedSubmission);
  };

  const updateProviderModelPreferences = (
    instanceId: ProviderInstanceId,
    next: {
      readonly hiddenModels: ReadonlyArray<string>;
      readonly modelOrder: ReadonlyArray<string>;
    },
  ) => {
    const hiddenModels = [...new Set(next.hiddenModels.filter((slug) => slug.trim().length > 0))];
    const modelOrder = [...new Set(next.modelOrder.filter((slug) => slug.trim().length > 0))];
    const rest = withoutProviderInstanceKey(settings.providerModelPreferences, instanceId);
    updateSettings({
      providerModelPreferences:
        hiddenModels.length === 0 && modelOrder.length === 0
          ? rest
          : {
              ...rest,
              [instanceId]: {
                hiddenModels,
                modelOrder,
              },
            },
    });
  };

  const updateProviderFavoriteModels = (
    instanceId: ProviderInstanceId,
    nextFavoriteModels: ReadonlyArray<string>,
  ) => {
    const favoriteModels = [
      ...new Set(
        Arr.filterMap(nextFavoriteModels, (slug) => {
          const trimmedSlug = slug.trim();
          return trimmedSlug.length > 0 ? Result.succeed(trimmedSlug) : Result.failVoid;
        }),
      ),
    ];
    updateSettings({
      favorites: [
        ...withoutProviderInstanceFavorites(settings.favorites ?? [], instanceId),
        ...favoriteModels.map((model) => ({ provider: instanceId, model })),
      ],
    });
  };

  const resetDefaultInstance = (driverKind: ProviderDriverKind) => {
    type LegacyProviderSettings = (typeof settings.providers)[keyof typeof settings.providers];
    const defaultLegacyProviders = DEFAULT_UNIFIED_SETTINGS.providers as Record<
      string,
      LegacyProviderSettings | undefined
    >;
    const defaultInstanceId = defaultInstanceIdForDriver(driverKind);
    const defaultLegacyProvider = defaultLegacyProviders[driverKind];
    if (defaultLegacyProvider === undefined) return;
    updateSettings({
      providers: {
        ...settings.providers,
        [driverKind]: defaultLegacyProvider,
      } as typeof settings.providers,
      providerInstances: withoutProviderInstanceKey(providerInstances, defaultInstanceId),
      providerModelPreferences: withoutProviderInstanceKey(
        settings.providerModelPreferences,
        defaultInstanceId,
      ),
      favorites: withoutProviderInstanceFavorites(settings.favorites ?? [], defaultInstanceId),
    });
  };

  return (
    <SettingsPageContainer>
      <SettingsSection
        title="Providers"
        contentVariant="stack"
        headerAction={
          <div className="flex items-center gap-1.5">
            <ProviderLastChecked lastCheckedAt={lastCheckedAt} />
            <Tooltip>
              <TooltipTrigger
                render={
                  <Button
                    hidden
                    size="icon-xs"
                    variant="ghost"
                    className="size-5 rounded-sm p-0 text-muted-foreground hover:text-foreground"
                    onClick={() => setIsAddInstanceDialogOpen(true)}
                    aria-label="Add provider instance"
                  >
                    <PlusIcon className="size-3" />
                  </Button>
                }
              />
              <TooltipPopup side="top">Add provider instance</TooltipPopup>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger
                render={
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    className="size-5 rounded-sm p-0 text-muted-foreground hover:text-foreground"
                    disabled={isRefreshingProviders}
                    onClick={() => void refreshProviders()}
                    aria-label="Refresh provider status"
                  >
                    {isRefreshingProviders ? (
                      <LoaderIcon className="size-3 animate-spin" />
                    ) : (
                      <RefreshCwIcon className="size-3" />
                    )}
                  </Button>
                }
              />
              <TooltipPopup side="top">Refresh provider status</TooltipPopup>
            </Tooltip>
          </div>
        }
      >
        {rows.map((row) => {
          const driverOption = getDriverOption(row.driver);
          const liveProvider = serverProviders.find(
            (candidate) => candidate.instanceId === row.instanceId,
          );
          const updateCandidate = liveProvider
            ? providerUpdateCandidateByInstanceId.get(liveProvider.instanceId)
            : undefined;
          const isDriverUpdateRunning =
            updateCandidate !== undefined &&
            (updatingProviderDrivers.has(updateCandidate.driver) ||
              serverProviders.some(
                (provider) =>
                  provider.driver === updateCandidate.driver && isProviderUpdateActive(provider),
              ));
          const showInlineUpdateButton =
            updateCandidate !== undefined &&
            hasOneClickUpdateProviderCandidate(updateCandidate, serverProviders);
          const canRunInlineUpdate =
            updateCandidate !== undefined &&
            canOneClickUpdateProviderCandidate(updateCandidate, serverProviders) &&
            !updatingProviderDrivers.has(updateCandidate.driver);
          const modelPreferences = settings.providerModelPreferences?.[row.instanceId] ?? {
            hiddenModels: [],
            modelOrder: [],
          };
          const favoriteModels = Arr.filterMap(settings.favorites ?? [], (favorite) =>
            favorite.provider === row.instanceId ? Result.succeed(favorite.model) : Result.failVoid,
          );
          const resetLabel = driverOption?.label ?? String(row.driver);
          const headerAction =
            row.isDefault && row.isDirty ? (
              <SettingResetButton
                label={`${resetLabel} provider settings`}
                onClick={() => resetDefaultInstance(row.driver)}
              />
            ) : null;
          return (
            <ProviderInstanceCard
              key={row.instanceId}
              instanceId={row.instanceId}
              instance={row.instance}
              driverOption={driverOption}
              liveProvider={liveProvider}
              isExpanded={openInstanceDetails[row.instanceId] ?? false}
              onExpandedChange={(open) =>
                setOpenInstanceDetails((existing) => ({
                  ...existing,
                  [row.instanceId]: open,
                }))
              }
              onUpdate={(next) => {
                const wasEnabled = row.instance.enabled ?? true;
                const isDisabling = next.enabled === false && wasEnabled;
                const isEnablingClaude =
                  row.driver === "claudeAgent" &&
                  wasEnabled === false &&
                  (next.enabled ?? true) === true;
                const shouldClearTextGen = isDisabling && textGenInstanceId === row.instanceId;
                updateProviderInstance(row, next, {
                  ...(shouldClearTextGen
                    ? {
                        textGenerationModelSelection:
                          DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection,
                      }
                    : {}),
                  ...(isEnablingClaude ? { onSuccess: refreshClaudeUsage } : {}),
                });
              }}
              onDelete={row.isDefault ? undefined : () => deleteProviderInstance(row.instanceId)}
              headerAction={headerAction}
              {...(row.isDefault
                ? {
                    sessionDefaults: providerSessionDefaults[row.driver],
                    onSessionDefaultsChange: (next: ProviderSessionDefault) =>
                      updateSessionDefaults(row.driver, next),
                  }
                : {})}
              hiddenModels={modelPreferences.hiddenModels}
              favoriteModels={favoriteModels}
              modelOrder={modelPreferences.modelOrder}
              onHiddenModelsChange={(hiddenModels) =>
                updateProviderModelPreferences(row.instanceId, {
                  ...modelPreferences,
                  hiddenModels,
                })
              }
              onFavoriteModelsChange={(favoriteModels) =>
                updateProviderFavoriteModels(row.instanceId, favoriteModels)
              }
              onModelOrderChange={(modelOrder) =>
                updateProviderModelPreferences(row.instanceId, {
                  ...modelPreferences,
                  modelOrder,
                })
              }
              onRunUpdate={
                showInlineUpdateButton && updateCandidate
                  ? () => {
                      if (!canRunInlineUpdate) {
                        return;
                      }
                      void runProviderUpdate(updateCandidate);
                    }
                  : undefined
              }
              onRecheck={refreshProviders}
              isUpdating={showInlineUpdateButton ? isDriverUpdateRunning : undefined}
            />
          );
        })}
      </SettingsSection>

      {isAddInstanceDialogOpen ? (
        <AddProviderInstanceDialog open onOpenChange={setIsAddInstanceDialogOpen} />
      ) : null}
    </SettingsPageContainer>
  );
}

export function ArchivedThreadsPanel() {
  const projects = useProjects();
  const serverConfigs = useServerConfigs();
  const {
    unarchiveThread,
    deleteThread,
    confirmAndDeleteThread,
    worktreeRemovalTarget,
    requestWorktreeRemoval,
    closeWorktreeRemovalDialog,
    completeWorktreeRemoval,
  } = useThreadActions();
  const environmentIds = useMemo(
    () => [...new Set(projects.map((project) => project.environmentId))],
    [projects],
  );
  const {
    snapshots: archivedSnapshots,
    error: archiveError,
    isLoading: isLoadingArchive,
    refresh: refreshArchivedThreads,
  } = useArchivedThreadSnapshots(environmentIds);

  const archivedGroups = useMemo(() => {
    const projectsByEnvironmentAndId = new Map(
      archivedSnapshots.flatMap(({ environmentId, snapshot }) =>
        snapshot.projects.map(
          (project) =>
            [
              `${environmentId}:${project.id}`,
              {
                id: project.id,
                environmentId,
                name: project.title,
                cwd: project.workspaceRoot,
              },
            ] as const,
        ),
      ),
    );
    // Closed chat panels are reopened from their host's + menu, not listed here.
    const threads = archivedSnapshots.flatMap(({ environmentId, snapshot }) =>
      snapshot.threads.flatMap((thread) =>
        thread.kind === "panel" ? [] : [{ ...thread, environmentId }],
      ),
    );

    const archivedProjects = Array.from(projectsByEnvironmentAndId.values());
    const groups: Array<{
      readonly project: (typeof archivedProjects)[number];
      readonly threads: Array<(typeof threads)[number]>;
    }> = [];
    for (const project of archivedProjects) {
      const projectThreads: Array<(typeof threads)[number]> = [];
      for (const thread of threads) {
        if (thread.projectId === project.id && thread.environmentId === project.environmentId) {
          projectThreads.push(thread);
        }
      }
      if (projectThreads.length > 0) {
        groups.push({
          project,
          threads: projectThreads.toSorted((left, right) => {
            const leftKey = left.archivedAt ?? left.createdAt;
            const rightKey = right.archivedAt ?? right.createdAt;
            return rightKey.localeCompare(leftKey) || right.id.localeCompare(left.id);
          }),
        });
      }
    }
    return groups;
  }, [archivedSnapshots]);

  const handleArchivedThreadContextMenu = useCallback(
    async (
      thread: OrchestrationThreadShell & { readonly environmentId: EnvironmentId },
      position: { x: number; y: number },
    ) => {
      const api = readLocalApi();
      if (!api) return;
      const threadRef = scopeThreadRef(thread.environmentId, thread.id);
      const clicked = await api.contextMenu.show(
        [
          { id: "unarchive", label: "Unarchive" },
          { id: "delete", label: "Delete", destructive: true },
        ],
        position,
      );

      if (clicked === "unarchive") {
        const result = await unarchiveThread(threadRef);
        if (result._tag === "Success") {
          refreshArchivedThreads();
        } else if (!isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Failed to unarchive thread",
              description: error instanceof Error ? error.message : "An error occurred.",
            }),
          );
        }
        return;
      }

      if (clicked === "delete") {
        const serverConfig = serverConfigs.get(thread.environmentId);
        const removalPolicy =
          serverConfig === undefined
            ? null
            : selectWorktreeCatalogCapabilityPolicy(serverConfig.environment).removal;
        if (thread.worktreePath && removalPolicy !== "legacy-detach-only") {
          requestWorktreeRemoval({
            environmentId: thread.environmentId,
            projectId: thread.projectId,
            threadId: thread.id,
            title: thread.title,
            path: thread.worktreePath,
            branch: thread.branch ?? null,
            availability: "verification-unavailable",
            registrationState: null,
            locked: false,
          });
          return;
        }
        const isLegacyWorktreeDetach =
          thread.worktreePath !== null &&
          thread.worktreePath !== undefined &&
          removalPolicy === "legacy-detach-only";
        if (isLegacyWorktreeDetach) {
          const confirmed = await api.dialogs.confirm(
            [
              `Remove worktree "${thread.title}" from BiBCode?`,
              "The Git worktree and its files will be left untouched.",
            ].join("\n"),
          );
          if (!confirmed) return;
        }
        const result = isLegacyWorktreeDetach
          ? await deleteThread(threadRef)
          : await confirmAndDeleteThread(threadRef);
        if (result._tag === "Success") {
          refreshArchivedThreads();
        } else if (!isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Failed to delete thread",
              description: error instanceof Error ? error.message : "An error occurred.",
            }),
          );
        }
      }
    },
    [
      confirmAndDeleteThread,
      deleteThread,
      refreshArchivedThreads,
      requestWorktreeRemoval,
      serverConfigs,
      unarchiveThread,
    ],
  );

  const handleWorktreeRemoved = useCallback(
    async (removedTarget: WorktreeRemovalTarget, result: WorktreeRemovalResult) => {
      const cleanupResult = await completeWorktreeRemoval(removedTarget, result);
      if (cleanupResult._tag === "Success") {
        refreshArchivedThreads();
        return;
      }
      if (isAtomCommandInterrupted(cleanupResult)) return;

      const error = squashAtomCommandFailure(cleanupResult);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Worktree removed, but navigation failed",
          description:
            error instanceof Error
              ? error.message
              : "Select another thread from the archived list to continue.",
        }),
      );
    },
    [completeWorktreeRemoval, refreshArchivedThreads],
  );

  return (
    <>
      <WorktreeRemovalDialog
        open={worktreeRemovalTarget !== null}
        target={worktreeRemovalTarget}
        onOpenChange={(open) => {
          if (!open) closeWorktreeRemovalDialog();
        }}
        onRemoved={(removedTarget, result) => {
          void handleWorktreeRemoved(removedTarget, result);
        }}
      />
      <SettingsPageContainer>
        {archivedGroups.length === 0 ? (
          <SettingsSection title="Archived threads">
            <SettingsRow
              title={
                <span className="inline-flex items-center gap-2">
                  {isLoadingArchive ? (
                    <LoaderIcon className="size-3.5 animate-spin text-muted-foreground" />
                  ) : (
                    <ArchiveIcon className="size-3.5 text-muted-foreground" />
                  )}
                  {isLoadingArchive
                    ? "Loading archived threads"
                    : archiveError
                      ? "Could not load archived threads"
                      : "No archived threads"}
                </span>
              }
              description={
                isLoadingArchive
                  ? "Checking connected environments."
                  : (archiveError ?? "Archived threads will appear here.")
              }
            />
          </SettingsSection>
        ) : (
          archivedGroups.map(({ project, threads: projectThreads }) => (
            <SettingsSection
              key={project.id}
              title={project.name}
              icon={<ProjectFavicon environmentId={project.environmentId} cwd={project.cwd} />}
            >
              {projectThreads.map((thread) => (
                <SettingsRow
                  key={thread.id}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    void (async () => {
                      const result = await settlePromise(() =>
                        handleArchivedThreadContextMenu(thread, {
                          x: event.clientX,
                          y: event.clientY,
                        }),
                      );
                      if (result._tag === "Failure") {
                        const error = squashAtomCommandFailure(result);
                        toastManager.add(
                          stackedThreadToast({
                            type: "error",
                            title: "Archived thread action failed",
                            description:
                              error instanceof Error ? error.message : "An error occurred.",
                          }),
                        );
                      }
                    })();
                  }}
                  title={thread.title}
                  description={
                    <>
                      Archived {formatRelativeTimeLabel(thread.archivedAt ?? thread.createdAt)}
                      {" \u00b7 Created "}
                      {formatRelativeTimeLabel(thread.createdAt)}
                    </>
                  }
                  control={
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 shrink-0 cursor-pointer gap-1.5 px-2.5"
                      onClick={() => {
                        void (async () => {
                          const result = await unarchiveThread(
                            scopeThreadRef(thread.environmentId, thread.id),
                          );
                          if (result._tag === "Success") {
                            refreshArchivedThreads();
                            return;
                          }
                          if (!isAtomCommandInterrupted(result)) {
                            const error = squashAtomCommandFailure(result);
                            toastManager.add(
                              stackedThreadToast({
                                type: "error",
                                title: "Failed to unarchive thread",
                                description:
                                  error instanceof Error ? error.message : "An error occurred.",
                              }),
                            );
                          }
                        })();
                      }}
                    >
                      <ArchiveX className="size-3.5" />
                      <span>Unarchive</span>
                    </Button>
                  }
                />
              ))}
            </SettingsSection>
          ))
        )}
      </SettingsPageContainer>
    </>
  );
}
