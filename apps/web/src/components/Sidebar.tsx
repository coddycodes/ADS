import {
  ArchiveIcon,
  ArrowUpDownIcon,
  ChevronRightIcon,
  ContainerIcon,
  EllipsisIcon,
  FolderPlusIcon,
  GitBranchIcon,
  GitPullRequestIcon,
  LoaderIcon,
  PlusIcon,
  SearchIcon,
  SettingsIcon,
  TriangleAlertIcon,
} from "lucide-react";
import {
  prStatusIndicator,
  resolveThreadPr,
  terminalStatusFromRunningIds,
} from "./ThreadStatusIndicators";
import { projectModuleRouteProjectKey } from "./pullRequests/shared/projectModuleRoute.logic";
import { ProjectFavicon } from "./ProjectFavicon";
import { CreateWorktreeDialog } from "./CreateWorktreeDialog";
import { useAtomValue } from "@effect/atom-react";
import { autoAnimate } from "@formkit/auto-animate";
import React, {
  type CSSProperties,
  createContext,
  useCallback,
  useContext,
  useEffect,
  memo,
  useMemo,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useShallow } from "zustand/react/shallow";
import {
  DndContext,
  type DragCancelEvent,
  type CollisionDetection,
  PointerSensor,
  type DragStartEvent,
  closestCorners,
  pointerWithin,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { restrictToFirstScrollableAncestor, restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import {
  DEFAULT_PROVIDER_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  DEFAULT_SERVER_SETTINGS,
  type DiscoveredLocalServer,
  EDITORS,
  type EditorId,
  type EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  type ScopedProjectRef,
  type ScopedThreadRef,
  type ResolvedKeybindingsConfig,
  type ServerConfig,
  type SidebarProjectGroupingMode,
  type VcsAdoptedWorktreeStatus,
  type WorktreeRemovalResult,
  ThreadId,
} from "@bibcode/contracts";
import {
  parseScopedThreadKey,
  scopedProjectKey,
  scopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@bibcode/client-runtime/environment";
import { safeErrorLogAttributes } from "@bibcode/client-runtime/errors";
import {
  isAtomCommandInterrupted,
  settlePromise,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import {
  selectWorktreeCatalogCapabilityPolicy,
  selectWorktreeWorkspaceActionsAvailable,
} from "@bibcode/client-runtime/state/worktrees";
import { Link, useLocation, useNavigate, useParams, useRouter } from "@tanstack/react-router";
import {
  MAX_SIDEBAR_THREAD_PREVIEW_COUNT,
  MIN_SIDEBAR_THREAD_PREVIEW_COUNT,
  type SidebarProjectSortOrder,
  type SidebarRepositorySortOrder,
  type SidebarThreadPreviewCount,
  type SidebarThreadSortOrder,
} from "@bibcode/contracts/settings";
import { isDesktopLocalConnectionTarget } from "../connection/desktopLocal";
import {
  resolveEnvironmentCompatVerdict,
  selectRemoteUpdateControlCapability,
} from "../connection/environmentCompat";
import { useDesktopLocalBootstraps } from "../connection/useDesktopLocalBootstraps";
import { isDesktopHost } from "../env";
import { APP_BASE_NAME, APP_STAGE_LABEL } from "../branding";
import { useOpenPrLink } from "../lib/openPullRequestLink";
import { isTerminalFocused } from "../lib/terminalFocus";
import { cn, isMacPlatform, newCommandId, newThreadId } from "../lib/utils";
import { resolveProviderSessionSelectionForInstance } from "../providerSessionSelection";
import { useSidebarWorkspaceMetaStore } from "../sidebarWorkspaceMetaStore";
import {
  useProject,
  useProjects,
  useActiveEnvironmentId,
  useServerConfigs,
  useThreadShells,
  useThreadShellsForProjectRefs,
} from "../state/entities";
import { useThreadHasTerminalSurface } from "../terminalSurfaceState";
import { openDiscoveredPort } from "./preview/openDiscoveredPort";
import { useAtomCommand } from "../state/use-atom-command";
import { previewEnvironment } from "../state/preview";
import {
  legacyProjectCwdPreferenceKey,
  resolveProjectExpanded,
  useUiStateStore,
} from "../uiStateStore";
import {
  resolveShortcutCommand,
  shortcutLabelForCommand,
  shouldShowThreadJumpHintsForModifiers,
  threadJumpCommandForIndex,
  threadJumpIndexFromCommand,
  threadTraversalDirectionFromCommand,
} from "../keybindings";
import { isModelPickerOpen } from "../modelPickerVisibility";
import { useShortcutModifierState } from "../shortcutModifierState";
import { readLocalApi } from "../localApi";
import { useComposerDraftStore } from "../composerDraftStore";
import { useDesktopUpdateState } from "../state/desktopUpdate";
import {
  remoteUpdateEnvironment,
  useRemoteUpdateCheckState,
  useRemoteUpdateRun,
  useRequestRemoteUpdateConfirmation,
} from "../state/remoteUpdates";
import { isRemoteUpdateRunActive } from "@bibcode/client-runtime/state/remoteUpdateCoordinator";
import { manualUpdateSteps, visibleRemoteUpdateRun } from "./settings/remoteUpdatePresentation";
import {
  ManualUpdateStepsDialog,
  type ManualUpdateStepsRequest,
} from "./settings/ManualUpdateStepsDialog";
import { projectDataSafetyStore } from "../state/projectDataSafety";

import { useThreadActions } from "../hooks/useThreadActions";
import { projectEnvironment } from "../state/projects";
import { useEnvironmentQuery } from "../state/query";
import {
  environmentAvailabilityCommands,
  shellEnvironment,
  useEnvironmentShellSummary,
} from "../state/shell";
import { threadEnvironment, useEnvironmentThread } from "../state/threads";
import { vcsEnvironment } from "../state/vcs";
import {
  type EnvironmentPresentation,
  useEnvironment,
  useEnvironments,
  usePrimaryEnvironmentId,
} from "../state/environments";
import {
  buildThreadRouteParams,
  resolveProjectRouteRef,
  resolveThreadRouteRef,
} from "../threadRoutes";
import { stackedThreadToast, toastManager } from "./ui/toast";
import { SettingsSidebarNav } from "./settings/SettingsSidebarNav";
import {
  gitManagerRepositoryUnavailableCopy,
  resolveGitManagerRepositoryUnavailable,
} from "./gitManager/gitManagerRepositoryAvailability";
import { AgentsNavRow } from "./sidebar/AgentsNavRow";
import { EnvironmentCardHeader } from "./sidebar/EnvironmentCardHeader";
import { EnvironmentContextCard } from "./sidebar/EnvironmentContextCard";
import {
  buildEnvironmentCardIdentities,
  groupProjectsByRepository,
  type EnvironmentCardIdentity,
  type RepositoryGroup,
} from "./sidebar/repositoryView.logic";
import { SidebarRepositoryGroup } from "./sidebar/SidebarRepositoryGroup";
import { SidebarViewToggle } from "./sidebar/SidebarViewToggle";
import { ServerUpdateBadge, serverUpdateStatusFromQuery } from "./settings/ServerUpdateBadge";
import {
  ENVIRONMENT_RAIL_WIDTH_PX,
  resolveAddProjectTargetLabel,
  selectRailVisibleEnvironmentIds,
  toEnvironmentRailCandidate,
} from "./sidebar/environmentRail.logic";
import { Kbd } from "./ui/kbd";
import {
  getArm64IntelBuildWarningDescription,
  getDesktopUpdateActionError,
  isDesktopUpdateButtonDisabled,
  resolveDesktopUpdateButtonAction,
  shouldShowArm64IntelBuildWarning,
  shouldToastDesktopUpdateActionResult,
} from "./desktopUpdate.logic";
import { UpdateProtectionDialog } from "./desktop/UpdateProtectionDialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "./ui/alert";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import {
  Menu,
  MenuGroup,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
} from "./ui/menu";
import {
  NumberField,
  NumberFieldDecrement,
  NumberFieldGroup,
  NumberFieldIncrement,
  NumberFieldInput,
} from "./ui/number-field";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "./ui/select";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./ui/tooltip";
import {
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  SidebarSeparator,
  SidebarTrigger,
  useSidebar,
} from "./ui/sidebar";
import { useThreadSelectionStore } from "../threadSelectionStore";
import { useOpenAddProjectCommandPalette } from "../commandPaletteContext";
import {
  getSidebarThreadIdsToPrewarm,
  orderRowsWithPins,
  resolveAdjacentThreadId,
  isContextMenuPointerDown,
  isTrailingDoubleClick,
  resolveSidebarProjectAvailability,
  resolveSidebarStageBadgeLabel,
  resolveWorkspaceBranchLabel,
  contextMenuAnchorForRect,
  orderItemsByPreferredIds,
  shouldClearThreadSelectionOnMouseDown,
  sortProjectsForSidebar,
  splitPrimaryAndWorkspaceThreads,
  useThreadJumpHintVisibility,
  isContextMenuShortcut,
  isKeyboardContextMenuEcho,
  isWorkspaceThreadRunning,
  isWorktreeSessionRunning,
  resolveWorkspaceCardAgeSource,
  resolveWorkspaceCardClassName,
  SIDEBAR_CARD_CLASS,
  resolveWorkspaceCardStatus,
  resolveWorkspaceDirty,
  shouldShowWorkspaceBranchText,
  summarizeWorkspaceChats,
  workspaceCheckoutKey,
  type WorkspaceCardStatus,
  type WorkspaceChatSummary,
  createWorkspaceStatusLookup,
  resolveHighestWorkspaceCardStatus,
  WORKSPACE_CARD_STATUS,
} from "./Sidebar.logic";
import { sortThreads } from "../lib/threadSort";
import { SidebarUpdatePill } from "./sidebar/SidebarUpdatePill";
import {
  buildMultiSelectMenu,
  buildPrimaryCardMenu,
  buildProjectHeaderMenu,
  buildWorkspaceCardMenu,
  describeUnavailableWorkspace,
  parseProjectHeaderSelection,
  WORKTREE_DELETE_BLOCKED_REASON,
} from "./sidebar/sidebarMenus.logic";
import { markKeyboardContextMenuOpened } from "../contextMenuKeyboard";
import { resolveAgentProvider } from "./sidebar/agentsSection.logic";
import {
  buildWorkspaceModelLabels,
  resolveWorkspaceCardBranchTooltip,
  resolveWorkspaceCardPreview,
  resolveWorkspaceModelLabel,
} from "./sidebar/workspaceCard.logic";
import {
  isWorkspaceCardControlTarget,
  WorkspaceCardBranchLine,
  WorkspaceCardDirtyDot,
  workspaceCardIds,
  WorkspaceCardMoreChats,
  WorkspaceCardPortsButton,
  WorkspaceCardPrButton,
  WorkspaceCardSessionLine,
  WorkspaceCardShell,
  WorkspaceCardStatusGlyph,
  WorkspaceCardTerminalIcon,
  WorkspaceCardTitleLine,
} from "./sidebar/WorkspaceCard";
import { useWorkspaceCardLookups } from "./sidebar/useWorkspaceCardLookups";
import { EMPTY_CARD_PORTS, EMPTY_CARD_TERMINALS } from "./sidebar/workspaceCardLookups";
import { useCopyToClipboard } from "~/hooks/useCopyToClipboard";
import { useIsMobile } from "~/hooks/useMediaQuery";
import { CommandDialogTrigger } from "./ui/command";
import {
  useClientSettings,
  usePrimarySettings,
  useUpdateClientSettings,
} from "~/hooks/useSettings";
import { primaryServerConfigAtom, primaryServerKeybindingsAtom } from "../state/server";
import {
  deriveLogicalProjectKeyFromSettings,
  derivePhysicalProjectKey,
  deriveProjectGroupingOverrideKey,
  getProjectOrderKey,
  selectProjectGroupingSettings,
  type ProjectGroupingSettings,
} from "../logicalProject";
import type { SidebarThreadSummary } from "../types";
import {
  buildPhysicalToLogicalProjectKeyMap,
  buildSidebarProjectSnapshots,
  type SidebarProjectGroupMember,
  type SidebarProjectSnapshot,
} from "../sidebarProjectGrouping";
import { SidebarProviderUpdatePill } from "./sidebar/SidebarProviderUpdatePill";
import {
  getSupportedWorktreeDiscoveryMembers,
  WorktreeDiscoverySection,
} from "./WorktreeDiscoverySection";
import { worktreeEnvironment } from "../state/worktrees";
import { getBulkThreadDeletionConfirmation } from "../worktreeCleanup";
import { WorktreeAvailabilityWarning } from "./WorktreeAvailabilityWarning";
import { WorktreeRemovalDialog, type WorktreeRemovalTarget } from "./WorktreeRemovalDialog";
import {
  ImportCliSessionsDialog,
  type ImportCliSessionsSummary,
  type ImportCliSessionsTarget,
} from "./ImportCliSessionsDialog";
import { SidebarProjectAvailability } from "./sidebar/SidebarProjectAvailability";
import { readCurrentEnvironmentPresentationPolicy } from "../connection/currentEnvironmentPresentation";
import { environmentConnectionActions } from "../connection/environmentPresentationPolicy";

function SidebarEnvironmentContextCard() {
  const activeEnvironmentId = useActiveEnvironmentId();
  const environment = useEnvironment(activeEnvironmentId);
  const remoteUpdateControl = selectRemoteUpdateControlCapability(
    environment?.serverConfig ?? null,
  );
  const updateEnvironmentId = remoteUpdateControl ? activeEnvironmentId : null;
  const run = useRemoteUpdateRun(updateEnvironmentId);
  const runActive = isRemoteUpdateRunActive(run);
  const requestUpdate = useRequestRemoteUpdateConfirmation();
  const [stepsRequest, setStepsRequest] = useState<ManualUpdateStepsRequest | null>(null);
  const updateQuery = useEnvironmentQuery(
    updateEnvironmentId === null || runActive
      ? null
      : remoteUpdateEnvironment.snapshot({ environmentId: updateEnvironmentId, input: {} }),
  );
  const updateCheck = useRemoteUpdateCheckState(updateEnvironmentId);
  const runCheck = useAtomCommand(remoteUpdateEnvironment.check, { reportFailure: false });
  const refreshUpdateStatus = updateQuery.refresh;
  const checkForUpdates = useCallback(
    async (environmentId: EnvironmentId) => {
      // The check records its own progress and failure in the shared check state; the
      // status is re-read either way so the badge shows what the host now reports.
      await runCheck({ environmentId, input: {} });
      refreshUpdateStatus();
    },
    [runCheck, refreshUpdateStatus],
  );

  const confirmUpdate = () => {
    if (environment === null || updateEnvironmentId === null) return;
    requestUpdate({
      environmentId: updateEnvironmentId,
      name: environment.label,
      targetVersion: updateQuery.data?.latestVersion ?? null,
      progress: environment.serverConfig?.environment.capabilities.remoteUpdateProgress ?? false,
    });
  };
  return (
    <>
      <EnvironmentContextCard
        updateInProgress={runActive}
        {...(updateEnvironmentId === null
          ? {}
          : {
              updateBadge: (
                <ServerUpdateBadge
                  {...serverUpdateStatusFromQuery(updateQuery, {
                    connected: environment?.connection.phase === "connected",
                    check: updateCheck,
                  })}
                  run={visibleRemoteUpdateRun(run, updateQuery.data)}
                  name={environment?.label ?? "The server"}
                  onUpdate={confirmUpdate}
                  onRetryRun={confirmUpdate}
                  onRetry={refreshUpdateStatus}
                  onCheckAgain={() => void checkForUpdates(updateEnvironmentId)}
                />
              ),
              onCheckForUpdates: (environmentId: EnvironmentId) =>
                void checkForUpdates(environmentId),
              onShowUpdateSteps: () => {
                if (environment === null) return;
                const descriptor = environment.serverConfig?.environment;
                setStepsRequest({
                  name: environment.label,
                  steps: manualUpdateSteps({
                    installKind:
                      updateQuery.data?.support.installKind ??
                      descriptor?.remoteUpdateSupport?.installKind ??
                      "unknown",
                    os: descriptor?.platform?.os ?? "unknown",
                    arch: descriptor?.platform?.arch ?? "other",
                    sshLaunched: environment.entry.target._tag === "SshConnectionTarget",
                    serverVersion:
                      updateQuery.data?.serverVersion ?? descriptor?.serverVersion ?? "unknown",
                  }),
                });
              },
            })}
      />
      <ManualUpdateStepsDialog request={stepsRequest} onClose={() => setStepsRequest(null)} />
    </>
  );
}

const WorktreeRemovalRequestContext = createContext<
  ((target: WorktreeRemovalTarget) => void) | null
>(null);

const SIDEBAR_SORT_LABELS: Record<SidebarProjectSortOrder, string> = {
  updated_at: "Last user message",
  created_at: "Created at",
  manual: "Manual",
};
const SIDEBAR_REPOSITORY_SORT_LABELS: Record<SidebarRepositorySortOrder, string> = {
  name: "Name",
  updated_at: "Last user message",
  created_at: "Created at",
};
const SIDEBAR_THREAD_SORT_LABELS: Record<SidebarThreadSortOrder, string> = {
  updated_at: "Last user message",
  created_at: "Created at",
};
const SIDEBAR_LIST_ANIMATION_OPTIONS = {
  duration: 180,
  easing: "ease-out",
} as const;
const EMPTY_THREAD_JUMP_LABELS = new Map<string, string>();
const EMPTY_EDITOR_IDS: ReadonlyArray<EditorId> = [];
const PROJECT_GROUPING_MODE_LABELS: Record<SidebarProjectGroupingMode, string> = {
  repository: "Group by repository",
  repository_path: "Group by repository path",
  separate: "Keep separate",
};
const SIDEBAR_ICON_ACTION_BUTTON_CLASS =
  "inline-flex h-6 min-w-6 cursor-pointer items-center justify-center rounded-md px-[calc(--spacing(1)-1px)] text-muted-foreground/60 hover:text-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring";

export function handleSidebarNavigationKeyDown(input: {
  readonly event: Pick<
    globalThis.KeyboardEvent,
    "defaultPrevented" | "repeat" | "preventDefault" | "stopPropagation"
  >;
  readonly resolveCommand: () => string | null;
  readonly orderedThreadKeys: readonly string[];
  readonly currentThreadKey: string | null;
  readonly jumpThreadKeys: readonly string[];
  readonly threadByKey: ReadonlyMap<string, SidebarThreadSummary>;
  readonly navigateToThread: (threadRef: ScopedThreadRef) => void;
}): boolean {
  if (input.event.defaultPrevented || input.event.repeat) {
    return false;
  }

  const command = input.resolveCommand();
  const traversalDirection = threadTraversalDirectionFromCommand(command);
  const targetThreadKey =
    traversalDirection !== null
      ? resolveAdjacentThreadId({
          threadIds: input.orderedThreadKeys,
          currentThreadId: input.currentThreadKey,
          direction: traversalDirection,
        })
      : (() => {
          const jumpIndex = threadJumpIndexFromCommand(command ?? "");
          return jumpIndex === null ? null : (input.jumpThreadKeys[jumpIndex] ?? null);
        })();
  if (!targetThreadKey) {
    return false;
  }

  const targetThread = input.threadByKey.get(targetThreadKey);
  if (!targetThread) {
    return false;
  }

  input.event.preventDefault();
  input.event.stopPropagation();
  input.navigateToThread(scopeThreadRef(targetThread.environmentId, targetThread.id));
  return true;
}

export function handleSidebarSelectionMouseDown(input: {
  readonly hasSelection: boolean;
  readonly target: EventTarget | null;
  readonly clearSelection: () => void;
}): boolean {
  if (!input.hasSelection) {
    return false;
  }
  const target = input.target instanceof HTMLElement ? input.target : null;
  if (!shouldClearThreadSelectionOnMouseDown(target)) {
    return false;
  }
  input.clearSelection();
  return true;
}

function SidebarThreadDetailPrewarmer({ threadRef }: { readonly threadRef: ScopedThreadRef }) {
  useEnvironmentThread(threadRef.environmentId, threadRef.threadId);
  return null;
}

function clampSidebarThreadPreviewCount(value: number): SidebarThreadPreviewCount {
  return Math.min(
    MAX_SIDEBAR_THREAD_PREVIEW_COUNT,
    Math.max(MIN_SIDEBAR_THREAD_PREVIEW_COUNT, value),
  ) as SidebarThreadPreviewCount;
}

function formatProjectMemberActionLabel(
  member: SidebarProjectGroupMember,
  groupedProjectCount: number,
): string {
  if (groupedProjectCount <= 1) {
    return member.title;
  }

  return member.environmentLabel
    ? `${member.environmentLabel} — ${member.workspaceRoot}`
    : member.workspaceRoot;
}

async function chooseProjectMember(
  members: readonly SidebarProjectGroupMember[],
  position: { readonly x: number; readonly y: number },
): Promise<SidebarProjectGroupMember | null> {
  if (members.length <= 1) {
    return members[0] ?? null;
  }
  const api = readLocalApi();
  if (!api) {
    return null;
  }
  const clickedResult = await settlePromise(() =>
    api.contextMenu.show(
      members.map((member) => ({
        id: member.physicalProjectKey,
        label: formatProjectMemberActionLabel(member, members.length),
      })),
      position,
    ),
  );
  if (clickedResult._tag === "Failure") {
    const error = squashAtomCommandFailure(clickedResult);
    toastManager.add(
      stackedThreadToast({
        type: "error",
        title: "Could not choose environment",
        description: error instanceof Error ? error.message : "An error occurred.",
      }),
    );
    return null;
  }
  return members.find((member) => member.physicalProjectKey === clickedResult.value) ?? null;
}

export function projectExpansionPreferenceKeys(project: SidebarProjectSnapshot): string[] {
  return [
    project.projectKey,
    ...(project.sharedExpansionKey ? [project.sharedExpansionKey] : []),
    ...project.memberProjects.map((member) => member.physicalProjectKey),
    ...legacyExpansionKeys(project),
  ];
}

/**
 * The keys a toggle writes. A Repositories-view card writes only its own physical keys and reads
 * the shared and legacy path keys as fallbacks: both can be shared with another environment's card
 * (the legacy key is the bare checkout path), so writing them would collapse siblings too.
 */
export function projectExpansionToggleKeys(project: SidebarProjectSnapshot): string[] {
  return [
    project.projectKey,
    ...project.memberProjects.map((member) => member.physicalProjectKey),
    ...(project.sharedExpansionKey ? [] : legacyExpansionKeys(project)),
  ];
}

function legacyExpansionKeys(project: SidebarProjectSnapshot): string[] {
  return project.memberProjects.map((member) =>
    legacyProjectCwdPreferenceKey(member.workspaceRoot),
  );
}

// An environment card has a two-line header, taller than the fixed `sm` button
// height; `h-auto` lets it grow while normal project rows keep their height.
export function projectHeaderButtonClassName(input: {
  readonly showsSandboxBadge: boolean;
  readonly isManualProjectSorting: boolean;
  readonly isEnvironmentCard: boolean;
}): string {
  return cn(
    "gap-2 px-2 py-1.5 text-left hover:bg-accent group-hover/project-header:bg-accent group-hover/project-header:text-sidebar-accent-foreground",
    input.showsSandboxBadge ? "pr-20" : "pr-14",
    input.isManualProjectSorting ? "cursor-grab active:cursor-grabbing" : "cursor-pointer",
    input.isEnvironmentCard && "h-auto",
  );
}

function projectGroupingModeDescription(mode: SidebarProjectGroupingMode): string {
  switch (mode) {
    case "repository":
      return "Projects from the same repository share one sidebar row.";
    case "repository_path":
      return "Projects group only when both the repository and repo-relative path match.";
    case "separate":
      return "Every project path gets its own sidebar row.";
  }
}

function buildThreadJumpLabelMap(input: {
  keybindings: ResolvedKeybindingsConfig;
  platform: string;
  terminalOpen: boolean;
  threadJumpCommandByKey: ReadonlyMap<
    string,
    NonNullable<ReturnType<typeof threadJumpCommandForIndex>>
  >;
}): ReadonlyMap<string, string> {
  if (input.threadJumpCommandByKey.size === 0) {
    return EMPTY_THREAD_JUMP_LABELS;
  }

  const shortcutLabelOptions = {
    platform: input.platform,
    context: {
      terminalFocus: false,
      terminalOpen: input.terminalOpen,
    },
  } as const;
  const mapping = new Map<string, string>();
  for (const [threadKey, command] of input.threadJumpCommandByKey) {
    const label = shortcutLabelForCommand(input.keybindings, command, shortcutLabelOptions);
    if (label) {
      mapping.set(threadKey, label);
    }
  }
  return mapping.size > 0 ? mapping : EMPTY_THREAD_JUMP_LABELS;
}

interface SidebarThreadRowProps {
  thread: SidebarThreadSummary;
  isPinned: boolean;
  isUnread: boolean;
  worktreeStatus: VcsAdoptedWorktreeStatus | null;
  runningTerminalIds: readonly string[];
  discoveredPorts: readonly DiscoveredLocalServer[];
  projectCwd: string | null;
  orderedProjectThreadKeys: readonly string[];
  isActive: boolean;
  jumpLabel: string | null;
  appSettingsConfirmThreadArchive: boolean;
  renamingThreadKey: string | null;
  renamingTitle: string;
  setRenamingTitle: (title: string) => void;
  startThreadRename: (threadKey: string, title: string) => void;
  renamingInputRef: React.RefObject<HTMLInputElement | null>;
  renamingCommittedRef: React.RefObject<boolean>;
  confirmingArchiveThreadKey: string | null;
  setConfirmingArchiveThreadKey: React.Dispatch<React.SetStateAction<string | null>>;
  confirmArchiveButtonRefs: React.RefObject<Map<string, HTMLButtonElement>>;
  handleThreadClick: (
    event: React.MouseEvent,
    threadRef: ScopedThreadRef,
    orderedProjectThreadKeys: readonly string[],
  ) => void;
  navigateToThread: (threadRef: ScopedThreadRef) => void;
  handleMultiSelectContextMenu: (position: { x: number; y: number }) => Promise<void>;
  handleThreadContextMenu: (
    threadRef: ScopedThreadRef,
    position: { x: number; y: number },
    worktreeStatus: VcsAdoptedWorktreeStatus | null,
    branchName: string | null,
  ) => Promise<void>;
  clearSelection: () => void;
  commitRename: (
    threadRef: ScopedThreadRef,
    newTitle: string,
    originalTitle: string,
  ) => Promise<void>;
  cancelRename: () => void;
  attemptArchiveThread: (threadRef: ScopedThreadRef) => Promise<void>;
  openPrLink: (event: React.MouseEvent<HTMLElement>, prUrl: string) => void;
  /** Line 3's model: the catalog short name, else the slug. */
  modelLabel: string;
  /** Unarchived panel chats open in this card's worktree. */
  moreChatsCount: number;
  moreChatsStatus: WorkspaceCardStatus | null;
}

/**
 * A fixed number of references/primitives; no array comparison per card.
 * Ordered keys and lookup results are shared by the list.
 */
function sameSidebarThreadRowProps(
  previous: SidebarThreadRowProps,
  next: SidebarThreadRowProps,
): boolean {
  for (const key of Object.keys(next) as Array<keyof SidebarThreadRowProps>) {
    if (!Object.is(previous[key], next[key])) return false;
  }
  return true;
}

export const SidebarThreadRow = memo(function SidebarThreadRow(props: SidebarThreadRowProps) {
  const {
    orderedProjectThreadKeys,
    isActive,
    jumpLabel,
    appSettingsConfirmThreadArchive,
    renamingThreadKey,
    renamingTitle,
    setRenamingTitle,
    startThreadRename,
    renamingInputRef,
    renamingCommittedRef,
    confirmingArchiveThreadKey,
    setConfirmingArchiveThreadKey,
    confirmArchiveButtonRefs,
    handleThreadClick,
    navigateToThread,
    handleMultiSelectContextMenu,
    handleThreadContextMenu,
    clearSelection,
    commitRename,
    cancelRename,
    attemptArchiveThread,
    openPrLink,
    thread,
    isPinned,
    isUnread,
    worktreeStatus,
    runningTerminalIds,
    discoveredPorts,
    modelLabel,
    moreChatsCount,
    moreChatsStatus,
  } = props;
  const idBase = useId();
  const cardIds = workspaceCardIds(idBase);
  const requestWorktreeRemoval = useContext(WorktreeRemovalRequestContext);
  const threadRef = useMemo(
    () => scopeThreadRef(thread.environmentId, thread.id),
    [thread.environmentId, thread.id],
  );
  const threadKey = scopedThreadKey(threadRef);
  const lastVisitedAt = useUiStateStore((state) => state.threadLastVisitedAtById[threadKey]);
  const isSelected = useThreadSelectionStore((state) => state.selectedThreadKeys.has(threadKey));
  const markWorkspaceRowRead = useSidebarWorkspaceMetaStore((state) => state.markRead);
  const isMobile = useIsMobile();
  const openPreview = useAtomCommand(previewEnvironment.open, {
    reportFailure: false,
  });
  // For grouped projects the thread may belong to another environment than the
  // representative project; read its own project cwd so VCS status (and PR
  // detection) queries the right path.
  const threadProject = useProject(
    useMemo(
      () => scopeProjectRef(thread.environmentId, thread.projectId),
      [thread.environmentId, thread.projectId],
    ),
  );
  const threadProjectCwd = threadProject?.workspaceRoot ?? null;
  const gitCwd = thread.worktreePath ?? threadProjectCwd ?? props.projectCwd;
  const workspaceActionsAvailable = selectWorktreeWorkspaceActionsAvailable(worktreeStatus);
  const refreshWorktreeCatalog = useAtomCommand(worktreeEnvironment.refresh, {
    reportFailure: false,
  });
  const gitStatus = useEnvironmentQuery(
    workspaceActionsAvailable && gitCwd !== null
      ? isActive
        ? vcsEnvironment.status({
            environmentId: thread.environmentId,
            input: { cwd: gitCwd },
          })
        : vcsEnvironment.summary({
            environmentId: thread.environmentId,
            input: { cwd: gitCwd },
          })
      : null,
  );
  const keyboardMenuOpenedAtRef = useRef(Number.NEGATIVE_INFINITY);

  const status = resolveWorkspaceCardStatus(thread, lastVisitedAt);
  const branchLabel = resolveWorkspaceBranchLabel(gitStatus.data, thread.branch);
  const showBranchText = shouldShowWorkspaceBranchText(branchLabel, thread.title);
  // The badge belongs to the branch the card names, not the recorded one, which
  // lags checkouts made while the thread was closed.
  const pr = resolveThreadPr(branchLabel, gitStatus.data);
  const prStatus = prStatusIndicator(pr, gitStatus.data?.sourceControlProvider);
  const dirty = resolveWorkspaceDirty(gitStatus.data);
  const terminalStatus = terminalStatusFromRunningIds(runningTerminalIds);
  const hasBranchLine =
    showBranchText ||
    prStatus !== null ||
    dirty ||
    terminalStatus !== null ||
    discoveredPorts.length > 0;
  const preview = resolveWorkspaceCardPreview(thread, status);
  const provider = resolveAgentProvider(thread.session?.providerName);
  const hasSessionLine =
    thread.session !== null || thread.unresolvedDelivery != null || preview.text !== null;
  const ageIso = resolveWorkspaceCardAgeSource(thread, status.kind === "working");
  const isThreadRunning = isWorkspaceThreadRunning(thread);
  const isConfirmingArchive = confirmingArchiveThreadKey === threadKey && !isThreadRunning;

  const handleOpenDiscoveredPort = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      const port = discoveredPorts[0];
      if (!port) return;
      event.preventDefault();
      event.stopPropagation();
      navigateToThread(threadRef);
      void (async () => {
        const result = await openDiscoveredPort({ threadRef, port, openPreview });
        if (result._tag === "Success" || isAtomCommandInterrupted(result)) {
          return;
        }
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Unable to open preview",
            description:
              error instanceof Error ? error.message : "The preview could not be opened.",
          }),
        );
      })();
    },
    [discoveredPorts, navigateToThread, openPreview, threadRef],
  );
  const clearConfirmingArchive = useCallback(() => {
    setConfirmingArchiveThreadKey((current) => (current === threadKey ? null : current));
  }, [setConfirmingArchiveThreadKey, threadKey]);
  const handleMouseLeave = useCallback(() => {
    clearConfirmingArchive();
  }, [clearConfirmingArchive]);
  const handleBlurCapture = useCallback(
    (event: React.FocusEvent<HTMLLIElement>) => {
      const currentTarget = event.currentTarget;
      requestAnimationFrame(() => {
        if (currentTarget.contains(document.activeElement)) {
          return;
        }
        clearConfirmingArchive();
      });
    },
    [clearConfirmingArchive],
  );
  const handleCardClick = useCallback(
    (event: React.MouseEvent<HTMLLIElement>) => {
      if (isWorkspaceCardControlTarget(event.target)) return;
      markWorkspaceRowRead(threadKey);
      handleThreadClick(event, threadRef, orderedProjectThreadKeys);
    },
    [handleThreadClick, markWorkspaceRowRead, orderedProjectThreadKeys, threadKey, threadRef],
  );
  const handleCardDoubleClick = useCallback(
    (event: React.MouseEvent<HTMLLIElement>) => {
      // Already renaming this card: a double-click outside the input must not
      // restart and discard the in-progress edit.
      if (renamingThreadKey === threadKey) return;
      // On mobile the first tap navigates and closes the sidebar sheet, so the
      // inline rename can't be shown. Renaming there stays on the menu.
      if (isMobile) return;
      // cmd/ctrl/shift double-clicks are multi-select intent, not rename.
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      // The card's own controls (PR number, ports, Archive) never start a rename.
      if (isWorkspaceCardControlTarget(event.target)) return;
      event.preventDefault();
      startThreadRename(threadKey, thread.title);
    },
    [isMobile, renamingThreadKey, startThreadRename, threadKey, thread.title],
  );
  const openCardMenu = useCallback(
    (position: { x: number; y: number }) => {
      const hasSelection = useThreadSelectionStore.getState().hasSelection();
      if (hasSelection && isSelected) {
        void (async () => {
          const result = await settlePromise(() => handleMultiSelectContextMenu(position));
          if (result._tag === "Failure") {
            const error = squashAtomCommandFailure(result);
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Thread action failed",
                description: error instanceof Error ? error.message : "An error occurred.",
              }),
            );
          }
        })();
        return;
      }

      if (hasSelection) {
        clearSelection();
      }
      void (async () => {
        const result = await settlePromise(() =>
          handleThreadContextMenu(threadRef, position, worktreeStatus, branchLabel),
        );
        if (result._tag === "Failure") {
          const error = squashAtomCommandFailure(result);
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Thread action failed",
              description: error instanceof Error ? error.message : "An error occurred.",
            }),
          );
        }
      })();
    },
    [
      branchLabel,
      clearSelection,
      handleMultiSelectContextMenu,
      handleThreadContextMenu,
      isSelected,
      threadRef,
      worktreeStatus,
    ],
  );
  const handleCardContextMenu = useCallback(
    (event: React.MouseEvent<HTMLLIElement>) => {
      event.preventDefault();
      // Chromium and WebView2 also send contextmenu for Shift+F10 and the Menu
      // key; the keydown handler already opened the menu at the card.
      if (isKeyboardContextMenuEcho(keyboardMenuOpenedAtRef.current, performance.now())) {
        return;
      }
      openCardMenu({ x: event.clientX, y: event.clientY });
    },
    [openCardMenu],
  );
  const handleCardKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLButtonElement>) => {
      if (!isContextMenuShortcut(event)) return;
      event.preventDefault();
      event.stopPropagation();
      keyboardMenuOpenedAtRef.current = markKeyboardContextMenuOpened();
      openCardMenu(contextMenuAnchorForRect(event.currentTarget.getBoundingClientRect()));
    },
    [openCardMenu],
  );
  const handlePrClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      if (!prStatus) return;
      openPrLink(event, prStatus.url);
    },
    [openPrLink, prStatus],
  );
  const handleRenameInputRef = useCallback(
    (element: HTMLInputElement | null) => {
      if (element && renamingInputRef.current !== element) {
        renamingInputRef.current = element;
        element.focus();
        element.select();
      }
    },
    [renamingInputRef],
  );
  const handleRenameInputChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      setRenamingTitle(event.target.value);
    },
    [setRenamingTitle],
  );
  const handleRenameInputKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      event.stopPropagation();
      if (event.key === "Enter") {
        event.preventDefault();
        renamingCommittedRef.current = true;
        void commitRename(threadRef, renamingTitle, thread.title);
      } else if (event.key === "Escape") {
        event.preventDefault();
        renamingCommittedRef.current = true;
        cancelRename();
      }
    },
    [cancelRename, commitRename, renamingCommittedRef, renamingTitle, thread.title, threadRef],
  );
  const handleRenameInputBlur = useCallback(() => {
    if (!renamingCommittedRef.current) {
      void commitRename(threadRef, renamingTitle, thread.title);
    }
  }, [commitRename, renamingCommittedRef, renamingTitle, thread.title, threadRef]);
  // Keep clicks and double-clicks inside the rename input from reaching the card.
  // Without stopping `dblclick`, selecting a word would restart the rename.
  const handleRenameInputClick = useCallback((event: React.MouseEvent<HTMLInputElement>) => {
    event.stopPropagation();
  }, []);
  const handleConfirmArchiveRef = (element: HTMLButtonElement | null) => {
    if (element) {
      confirmArchiveButtonRefs.current.set(threadKey, element);
    } else {
      confirmArchiveButtonRefs.current.delete(threadKey);
    }
  };
  const stopPropagationOnPointerDown = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      event.stopPropagation();
    },
    [],
  );
  const handleConfirmArchiveClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      clearConfirmingArchive();
      void attemptArchiveThread(threadRef);
    },
    [attemptArchiveThread, clearConfirmingArchive, threadRef],
  );
  const handleStartArchiveConfirmation = (event: React.MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setConfirmingArchiveThreadKey(threadKey);
    requestAnimationFrame(() => {
      confirmArchiveButtonRefs.current.get(threadKey)?.focus();
    });
  };
  const handleArchiveImmediateClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      void attemptArchiveThread(threadRef);
    },
    [attemptArchiveThread, threadRef],
  );

  const archiveRevealClassName =
    "pointer-events-none -my-0.5 opacity-0 transition-opacity duration-150 max-sm:pointer-events-auto max-sm:opacity-100 group-hover/menu-sub-item:pointer-events-auto group-hover/menu-sub-item:opacity-100 group-focus-within/menu-sub-item:pointer-events-auto group-focus-within/menu-sub-item:opacity-100";
  const archiveButton = (onClick: (event: React.MouseEvent<HTMLButtonElement>) => void) => (
    <button
      type="button"
      data-card-control
      data-thread-selection-safe
      data-testid={`thread-archive-${thread.id}`}
      aria-label={`Archive ${thread.title}`}
      className={SIDEBAR_ICON_ACTION_BUTTON_CLASS}
      onPointerDown={stopPropagationOnPointerDown}
      onClick={onClick}
    >
      <ArchiveIcon className="size-3.5" />
    </button>
  );
  const trailing = jumpLabel ? (
    <span
      aria-label={jumpLabel}
      className="inline-flex h-5 shrink-0 items-center rounded-full border border-border/80 bg-background/90 px-1.5 font-mono text-xs font-medium text-foreground shadow-sm"
    >
      {jumpLabel}
    </span>
  ) : isConfirmingArchive ? (
    <button
      ref={handleConfirmArchiveRef}
      type="button"
      data-card-control
      data-thread-selection-safe
      data-testid={`thread-archive-confirm-${thread.id}`}
      aria-label={`Confirm archive ${thread.title}`}
      className="pointer-events-auto -my-0.5 inline-flex h-5 shrink-0 cursor-pointer items-center rounded-md bg-destructive/12 px-2 text-xs font-medium text-destructive transition-colors hover:bg-destructive/18 focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-destructive/40"
      onPointerDown={stopPropagationOnPointerDown}
      onClick={handleConfirmArchiveClick}
    >
      Confirm
    </button>
  ) : isThreadRunning ? null : appSettingsConfirmThreadArchive ? (
    <span className={archiveRevealClassName}>{archiveButton(handleStartArchiveConfirmation)}</span>
  ) : (
    <Tooltip>
      <TooltipTrigger render={<span className={archiveRevealClassName} />}>
        {archiveButton(handleArchiveImmediateClick)}
      </TooltipTrigger>
      <TooltipPopup side="top">Archive</TooltipPopup>
    </Tooltip>
  );

  return (
    <WorkspaceCardShell
      testId={`thread-row-${thread.id}`}
      buttonTestId={`thread-card-button-${thread.id}`}
      className={resolveWorkspaceCardClassName({ isActive, isSelected })}
      idBase={idBase}
      isActive={isActive}
      hasFlags={isUnread || isPinned}
      hasBranchLine={hasBranchLine}
      hasSessionLine={hasSessionLine}
      status={status}
      onClick={handleCardClick}
      onDoubleClick={handleCardDoubleClick}
      onContextMenu={handleCardContextMenu}
      onMouseLeave={handleMouseLeave}
      onBlurCapture={handleBlurCapture}
      onButtonKeyDown={handleCardKeyDown}
      footer={
        worktreeStatus && worktreeStatus.availability !== "present" ? (
          <WorktreeAvailabilityWarning
            status={worktreeStatus}
            onRetry={() => {
              void refreshWorktreeCatalog({
                environmentId: thread.environmentId,
                input: { projectId: thread.projectId },
              });
            }}
            onRemove={() => {
              requestWorktreeRemoval?.({
                environmentId: thread.environmentId,
                projectId: thread.projectId,
                threadId: thread.id,
                title: thread.title,
                path: worktreeStatus.path,
                branch: worktreeStatus.branch ?? thread.branch,
                availability: worktreeStatus.availability,
                registrationState: worktreeStatus.registrationState,
                locked: worktreeStatus.locked,
                ...(worktreeStatus.lockReason ? { lockReason: worktreeStatus.lockReason } : {}),
              });
            }}
          />
        ) : null
      }
    >
      <WorkspaceCardTitleLine
        id={cardIds.title}
        flagsId={cardIds.flags}
        title={thread.title}
        titleTestId={`thread-title-${thread.id}`}
        unread={isUnread}
        pinned={isPinned}
        pinnedTestId={`thread-pinned-${thread.id}`}
        renameInput={
          renamingThreadKey === threadKey ? (
            <input
              ref={handleRenameInputRef}
              data-card-control
              aria-label={`Rename ${thread.title}`}
              className="pointer-events-auto min-w-0 flex-1 truncate rounded border border-ring bg-transparent px-0.5 text-base outline-none sm:text-[13px]"
              value={renamingTitle}
              onChange={handleRenameInputChange}
              onKeyDown={handleRenameInputKeyDown}
              onBlur={handleRenameInputBlur}
              onClick={handleRenameInputClick}
              onDoubleClick={handleRenameInputClick}
            />
          ) : undefined
        }
        trailing={trailing}
      />
      {hasBranchLine ? (
        <WorkspaceCardBranchLine
          id={cardIds.branch}
          branch={showBranchText ? branchLabel : null}
          branchTooltip={
            showBranchText
              ? resolveWorkspaceCardBranchTooltip({
                  branch: branchLabel,
                  worktreePath: thread.worktreePath,
                  checkoutPath: gitCwd,
                })
              : null
          }
        >
          {prStatus ? <WorkspaceCardPrButton indicator={prStatus} onClick={handlePrClick} /> : null}
          {dirty ? <WorkspaceCardDirtyDot /> : null}
          {terminalStatus ? (
            <WorkspaceCardTerminalIcon
              label={terminalStatus.label}
              colorClass={terminalStatus.colorClass}
            />
          ) : null}
          <WorkspaceCardPortsButton ports={discoveredPorts} onClick={handleOpenDiscoveredPort} />
        </WorkspaceCardBranchLine>
      ) : null}
      {hasSessionLine ? (
        <WorkspaceCardSessionLine
          id={cardIds.session}
          provider={provider}
          preview={preview}
          model={modelLabel}
          ageIso={ageIso}
        />
      ) : null}
      {moreChatsCount > 0 && moreChatsStatus !== null ? (
        <WorkspaceCardMoreChats count={moreChatsCount} status={moreChatsStatus} />
      ) : null}
    </WorkspaceCardShell>
  );
}, sameSidebarThreadRowProps);

interface SidebarProjectThreadListProps {
  projectExpanded: boolean;
  hasOverflowingThreads: boolean;
  hiddenThreadStatus: WorkspaceCardStatus | null;
  orderedProjectThreadKeys: readonly string[];
  renderedThreads: readonly SidebarThreadSummary[];
  shouldShowThreadPanel: boolean;
  isThreadListExpanded: boolean;
  activeRouteThreadKey: string | null;
  threadJumpLabelByKey: ReadonlyMap<string, string>;
  appSettingsConfirmThreadArchive: boolean;
  renamingThreadKey: string | null;
  renamingTitle: string;
  setRenamingTitle: (title: string) => void;
  startThreadRename: (threadKey: string, title: string) => void;
  renamingInputRef: React.RefObject<HTMLInputElement | null>;
  renamingCommittedRef: React.RefObject<boolean>;
  confirmingArchiveThreadKey: string | null;
  setConfirmingArchiveThreadKey: React.Dispatch<React.SetStateAction<string | null>>;
  confirmArchiveButtonRefs: React.RefObject<Map<string, HTMLButtonElement>>;
  attachThreadListAutoAnimateRef: (node: HTMLElement | null) => void;
  handleThreadClick: (
    event: React.MouseEvent,
    threadRef: ScopedThreadRef,
    orderedProjectThreadKeys: readonly string[],
  ) => void;
  navigateToThread: (threadRef: ScopedThreadRef) => void;
  handleMultiSelectContextMenu: (position: { x: number; y: number }) => Promise<void>;
  handleThreadContextMenu: (
    threadRef: ScopedThreadRef,
    position: { x: number; y: number },
    worktreeStatus: VcsAdoptedWorktreeStatus | null,
    branchName: string | null,
  ) => Promise<void>;
  clearSelection: () => void;
  commitRename: (
    threadRef: ScopedThreadRef,
    newTitle: string,
    originalTitle: string,
  ) => Promise<void>;
  cancelRename: () => void;
  attemptArchiveThread: (threadRef: ScopedThreadRef) => Promise<void>;
  openPrLink: (event: React.MouseEvent<HTMLElement>, prUrl: string) => void;
  expandThreadListForProject: (projectKey: string) => void;
  collapseThreadListForProject: (projectKey: string) => void;
  project: SidebarProjectSnapshot;
  serverConfigs: ReadonlyMap<EnvironmentId, ServerConfig>;
  primaryThread: SidebarThreadSummary | null;
  modelLabels: ReadonlyMap<string, string>;
  chatSummaries: ReadonlyMap<string, WorkspaceChatSummary>;
  onPrimaryClick: () => void;
  openPrimaryCardMenu: (position: { x: number; y: number }, branchName: string | null) => void;
  showDiscovery: boolean;
  primaryEnvironmentId: EnvironmentId | null;
  onDiscoveryHiddenCountChange: (count: number | null) => void;
}

interface SidebarPrimaryCardProps {
  project: SidebarProjectSnapshot;
  primaryThread: SidebarThreadSummary | null;
  isActive: boolean;
  isPinned: boolean;
  isUnread: boolean;
  runningTerminalIds: readonly string[];
  discoveredPorts: readonly DiscoveredLocalServer[];
  modelLabel: string;
  moreChatsCount: number;
  moreChatsStatus: WorkspaceCardStatus | null;
  onClick: () => void;
  onOpenMenu: (position: { x: number; y: number }, branchName: string | null) => void;
  openPrLink: (event: React.MouseEvent<HTMLElement>, prUrl: string) => void;
  navigateToThread: (threadRef: ScopedThreadRef) => void;
}

/**
 * The primary card: the project's main checkout. Its title is the checkout's
 * live branch, and its PR and dirty state come straight from the passive
 * summary. The default thread's `branch` is only a fallback: the chat header
 * syncs it while the thread is open, so it lags checkouts made elsewhere.
 */
const SidebarPrimaryCard = memo(function SidebarPrimaryCard(props: SidebarPrimaryCardProps) {
  const {
    project,
    primaryThread,
    isActive,
    isPinned,
    isUnread,
    runningTerminalIds,
    discoveredPorts,
    modelLabel,
    moreChatsCount,
    moreChatsStatus,
    onClick,
    onOpenMenu,
    openPrLink,
    navigateToThread,
  } = props;
  const idBase = useId();
  const cardIds = workspaceCardIds(idBase);
  const gitStatus = useEnvironmentQuery(
    vcsEnvironment.summary({
      environmentId: project.environmentId,
      input: { cwd: project.workspaceRoot },
    }),
  );
  const summary = gitStatus.data;
  const unavailable = resolveGitManagerRepositoryUnavailable(summary ?? null);
  const unavailableCopy = unavailable
    ? gitManagerRepositoryUnavailableCopy(unavailable, project.workspaceRoot)
    : null;
  const liveBranch = resolveWorkspaceBranchLabel(summary, null);
  const title = unavailable
    ? project.displayName
    : (liveBranch ?? primaryThread?.branch ?? project.displayName);
  const branchName = unavailable ? null : (liveBranch ?? primaryThread?.branch ?? null);
  const primaryThreadRef = useMemo(
    () => (primaryThread ? scopeThreadRef(primaryThread.environmentId, primaryThread.id) : null),
    [primaryThread],
  );
  const primaryThreadKey = primaryThreadRef ? scopedThreadKey(primaryThreadRef) : null;
  // Today's row skipped the visit marker, so the primary never showed "Completed".
  const lastVisitedAt = useUiStateStore((state) =>
    primaryThreadKey === null ? undefined : state.threadLastVisitedAtById[primaryThreadKey],
  );
  const markRead = useSidebarWorkspaceMetaStore((state) => state.markRead);
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: false });
  const keyboardMenuOpenedAtRef = useRef(Number.NEGATIVE_INFINITY);

  const status = primaryThread
    ? resolveWorkspaceCardStatus(primaryThread, lastVisitedAt)
    : WORKSPACE_CARD_STATUS.idle;
  const prStatus = prStatusIndicator(summary?.pr ?? null, summary?.sourceControlProvider);
  const dirty = resolveWorkspaceDirty(summary);
  const terminalStatus = terminalStatusFromRunningIds(runningTerminalIds);
  const showBranchText = shouldShowWorkspaceBranchText(branchName, title);
  const hasBranchLine =
    unavailableCopy !== null ||
    showBranchText ||
    prStatus !== null ||
    dirty ||
    terminalStatus !== null ||
    discoveredPorts.length > 0;
  const preview = primaryThread
    ? resolveWorkspaceCardPreview(primaryThread, status)
    : { text: null, tone: null };
  const provider = resolveAgentProvider(primaryThread?.session?.providerName);
  const hasSessionLine =
    primaryThread !== null &&
    (primaryThread.session !== null ||
      primaryThread.unresolvedDelivery != null ||
      preview.text !== null);
  const ageIso = primaryThread
    ? resolveWorkspaceCardAgeSource(primaryThread, status.kind === "working")
    : null;

  const handleClick = useCallback(
    (event: React.MouseEvent<HTMLLIElement>) => {
      if (isWorkspaceCardControlTarget(event.target)) return;
      if (primaryThreadKey) {
        markRead(primaryThreadKey);
      }
      onClick();
    },
    [markRead, onClick, primaryThreadKey],
  );
  const handleContextMenu = useCallback(
    (event: React.MouseEvent<HTMLLIElement>) => {
      event.preventDefault();
      if (isKeyboardContextMenuEcho(keyboardMenuOpenedAtRef.current, performance.now())) {
        return;
      }
      onOpenMenu({ x: event.clientX, y: event.clientY }, branchName);
    },
    [branchName, onOpenMenu],
  );
  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLButtonElement>) => {
      if (!isContextMenuShortcut(event)) return;
      event.preventDefault();
      event.stopPropagation();
      keyboardMenuOpenedAtRef.current = markKeyboardContextMenuOpened();
      onOpenMenu(contextMenuAnchorForRect(event.currentTarget.getBoundingClientRect()), branchName);
    },
    [branchName, onOpenMenu],
  );
  const handlePrClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      if (prStatus) {
        openPrLink(event, prStatus.url);
      }
    },
    [openPrLink, prStatus],
  );
  const handleOpenDiscoveredPort = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      const port = discoveredPorts[0];
      if (!port || !primaryThreadRef) return;
      event.preventDefault();
      event.stopPropagation();
      navigateToThread(primaryThreadRef);
      void (async () => {
        const result = await openDiscoveredPort({ threadRef: primaryThreadRef, port, openPreview });
        if (result._tag === "Success" || isAtomCommandInterrupted(result)) {
          return;
        }
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Unable to open preview",
            description:
              error instanceof Error ? error.message : "The preview could not be opened.",
          }),
        );
      })();
    },
    [discoveredPorts, navigateToThread, openPreview, primaryThreadRef],
  );

  return (
    <WorkspaceCardShell
      testId={`primary-card-${project.id}`}
      buttonTestId={`primary-card-button-${project.id}`}
      className={resolveWorkspaceCardClassName({ isActive, isSelected: false })}
      idBase={idBase}
      isActive={isActive}
      hasFlags={isUnread || isPinned}
      hasBranchLine={hasBranchLine}
      hasSessionLine={hasSessionLine}
      status={status}
      onClick={handleClick}
      onContextMenu={handleContextMenu}
      onButtonKeyDown={handleKeyDown}
    >
      <WorkspaceCardTitleLine
        id={cardIds.title}
        flagsId={cardIds.flags}
        title={title}
        titleTestId={`primary-card-title-${project.id}`}
        unread={isUnread}
        pinned={isPinned}
        primary
      />
      {hasBranchLine ? (
        <WorkspaceCardBranchLine
          id={cardIds.branch}
          branch={showBranchText ? branchName : null}
          notice={
            unavailableCopy
              ? { label: unavailableCopy.shortLabel, description: unavailableCopy.message }
              : null
          }
          branchTooltip={
            showBranchText
              ? resolveWorkspaceCardBranchTooltip({
                  branch: branchName,
                  worktreePath: null,
                  checkoutPath: project.workspaceRoot,
                })
              : null
          }
        >
          {prStatus ? <WorkspaceCardPrButton indicator={prStatus} onClick={handlePrClick} /> : null}
          {dirty ? <WorkspaceCardDirtyDot /> : null}
          {terminalStatus ? (
            <WorkspaceCardTerminalIcon
              label={terminalStatus.label}
              colorClass={terminalStatus.colorClass}
            />
          ) : null}
          <WorkspaceCardPortsButton ports={discoveredPorts} onClick={handleOpenDiscoveredPort} />
        </WorkspaceCardBranchLine>
      ) : null}
      {hasSessionLine ? (
        <WorkspaceCardSessionLine
          id={cardIds.session}
          provider={provider}
          preview={preview}
          model={modelLabel}
          ageIso={ageIso}
        />
      ) : null}
      {moreChatsCount > 0 && moreChatsStatus !== null ? (
        <WorkspaceCardMoreChats count={moreChatsCount} status={moreChatsStatus} />
      ) : null}
    </WorkspaceCardShell>
  );
});

const SidebarProjectThreadList = memo(function SidebarProjectThreadList(
  props: SidebarProjectThreadListProps,
) {
  const {
    projectExpanded,
    hasOverflowingThreads,
    hiddenThreadStatus,
    orderedProjectThreadKeys,
    renderedThreads,
    shouldShowThreadPanel,
    isThreadListExpanded,
    activeRouteThreadKey,
    threadJumpLabelByKey,
    appSettingsConfirmThreadArchive,
    renamingThreadKey,
    renamingTitle,
    setRenamingTitle,
    startThreadRename,
    renamingInputRef,
    renamingCommittedRef,
    confirmingArchiveThreadKey,
    setConfirmingArchiveThreadKey,
    confirmArchiveButtonRefs,
    attachThreadListAutoAnimateRef,
    handleThreadClick,
    navigateToThread,
    handleMultiSelectContextMenu,
    handleThreadContextMenu,
    clearSelection,
    commitRename,
    cancelRename,
    attemptArchiveThread,
    openPrLink,
    expandThreadListForProject,
    collapseThreadListForProject,
    project,
    serverConfigs,
    primaryThread,
    modelLabels,
    chatSummaries,
    onPrimaryClick,
    openPrimaryCardMenu,
    showDiscovery,
    primaryEnvironmentId,
    onDiscoveryHiddenCountChange,
  } = props;
  const pinnedKeys = useSidebarWorkspaceMetaStore((state) => state.pinnedThreadKeys);
  const unreadKeys = useSidebarWorkspaceMetaStore((state) => state.unreadThreadKeys);
  const lookupThreads = useMemo(
    () => (primaryThread ? [primaryThread, ...renderedThreads] : renderedThreads),
    [primaryThread, renderedThreads],
  );
  const cardLookups = useWorkspaceCardLookups({
    project,
    serverConfigs,
    active: shouldShowThreadPanel,
    threads: lookupThreads,
    pinnedKeys,
    unreadKeys,
  });
  const projectKey = project.projectKey;
  const projectCwd = project.workspaceRoot;
  const primaryThreadKey = primaryThread
    ? scopedThreadKey(scopeThreadRef(primaryThread.environmentId, primaryThread.id))
    : null;
  const primaryKeyInfo = primaryThread ? cardLookups.keys.get(primaryThread) : undefined;
  const primaryThreadActive =
    primaryThreadKey !== null && activeRouteThreadKey === primaryThreadKey;
  const primaryChats = chatSummaries.get(
    workspaceCheckoutKey({
      environmentId: primaryThread?.environmentId ?? project.environmentId,
      projectId: primaryThread?.projectId ?? project.id,
      worktreePath: null,
    }),
  );
  const showMoreButtonRender = useMemo(() => <button type="button" />, []);
  const showLessButtonRender = useMemo(() => <button type="button" />, []);

  return (
    <SidebarMenuSub
      ref={attachThreadListAutoAnimateRef}
      className="mx-0.5 my-0 w-full translate-x-0 gap-0.5 overflow-hidden px-1 sm:mx-1 sm:px-1.5"
    >
      {projectExpanded && showDiscovery ? (
        <WorktreeDiscoverySection
          project={project}
          serverConfigs={serverConfigs}
          primaryEnvironmentId={primaryEnvironmentId}
          onNavigateToThread={navigateToThread}
          onHiddenCountChange={onDiscoveryHiddenCountChange}
        />
      ) : null}
      {projectExpanded || primaryThreadActive ? (
        <SidebarPrimaryCard
          project={project}
          primaryThread={primaryThread}
          isPinned={primaryKeyInfo?.pinned ?? false}
          isUnread={primaryKeyInfo?.unread ?? false}
          runningTerminalIds={
            primaryThreadKey
              ? (cardLookups.terminals.get(primaryThreadKey) ?? EMPTY_CARD_TERMINALS)
              : EMPTY_CARD_TERMINALS
          }
          discoveredPorts={
            primaryThreadKey
              ? (cardLookups.ports.get(primaryThreadKey) ?? EMPTY_CARD_PORTS)
              : EMPTY_CARD_PORTS
          }
          isActive={primaryThreadActive}
          modelLabel={primaryThread ? resolveWorkspaceModelLabel(modelLabels, primaryThread) : ""}
          moreChatsCount={primaryChats?.count ?? 0}
          moreChatsStatus={primaryChats?.status ?? null}
          onClick={onPrimaryClick}
          onOpenMenu={openPrimaryCardMenu}
          openPrLink={openPrLink}
          navigateToThread={navigateToThread}
        />
      ) : null}
      {shouldShowThreadPanel &&
        renderedThreads.map((thread) => {
          const keyInfo = cardLookups.keys.get(thread)!;
          const threadKey = keyInfo.key;
          const chatSummary =
            thread.worktreePath === null
              ? undefined
              : chatSummaries.get(workspaceCheckoutKey(thread));
          return (
            <SidebarThreadRow
              key={threadKey}
              thread={thread}
              isPinned={keyInfo.pinned}
              isUnread={keyInfo.unread}
              worktreeStatus={
                thread.worktreePath ? (cardLookups.adopted.get(threadKey) ?? null) : null
              }
              runningTerminalIds={cardLookups.terminals.get(threadKey) ?? EMPTY_CARD_TERMINALS}
              discoveredPorts={cardLookups.ports.get(threadKey) ?? EMPTY_CARD_PORTS}
              projectCwd={projectCwd}
              orderedProjectThreadKeys={orderedProjectThreadKeys}
              isActive={activeRouteThreadKey === threadKey}
              jumpLabel={threadJumpLabelByKey.get(threadKey) ?? null}
              appSettingsConfirmThreadArchive={appSettingsConfirmThreadArchive}
              renamingThreadKey={renamingThreadKey}
              renamingTitle={renamingThreadKey === threadKey ? renamingTitle : ""}
              setRenamingTitle={setRenamingTitle}
              startThreadRename={startThreadRename}
              renamingInputRef={renamingInputRef}
              renamingCommittedRef={renamingCommittedRef}
              confirmingArchiveThreadKey={confirmingArchiveThreadKey}
              setConfirmingArchiveThreadKey={setConfirmingArchiveThreadKey}
              confirmArchiveButtonRefs={confirmArchiveButtonRefs}
              handleThreadClick={handleThreadClick}
              navigateToThread={navigateToThread}
              handleMultiSelectContextMenu={handleMultiSelectContextMenu}
              handleThreadContextMenu={handleThreadContextMenu}
              clearSelection={clearSelection}
              commitRename={commitRename}
              cancelRename={cancelRename}
              attemptArchiveThread={attemptArchiveThread}
              openPrLink={openPrLink}
              modelLabel={resolveWorkspaceModelLabel(modelLabels, thread)}
              moreChatsCount={chatSummary?.count ?? 0}
              moreChatsStatus={chatSummary?.status ?? null}
            />
          );
        })}

      {projectExpanded && hasOverflowingThreads && !isThreadListExpanded && (
        <SidebarMenuSubItem className="w-full">
          <SidebarMenuSubButton
            render={showMoreButtonRender}
            data-thread-selection-safe
            size="sm"
            className="h-6 w-full translate-x-0 justify-start px-2 text-left text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
            onClick={() => {
              expandThreadListForProject(projectKey);
            }}
          >
            <span className="flex min-w-0 flex-1 items-center gap-2">
              {hiddenThreadStatus ? <WorkspaceCardStatusGlyph status={hiddenThreadStatus} /> : null}
              <span>Show more</span>
            </span>
          </SidebarMenuSubButton>
        </SidebarMenuSubItem>
      )}
      {projectExpanded && hasOverflowingThreads && isThreadListExpanded && (
        <SidebarMenuSubItem className="w-full">
          <SidebarMenuSubButton
            render={showLessButtonRender}
            data-thread-selection-safe
            size="sm"
            className="h-6 w-full translate-x-0 justify-start px-2 text-left text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
            onClick={() => {
              collapseThreadListForProject(projectKey);
            }}
          >
            <span>Show less</span>
          </SidebarMenuSubButton>
        </SidebarMenuSubItem>
      )}
    </SidebarMenuSub>
  );
});

interface SidebarProjectItemProps {
  project: SidebarProjectSnapshot;
  isThreadListExpanded: boolean;
  activeRouteThreadKey: string | null;
  moduleRouteActive: boolean;
  selectedProjectKey: string | null;
  selectProject: (projectKey: string) => void;
  openCreateWorktreeDialog: (projectRef?: ScopedProjectRef | null) => void;
  archiveThread: ReturnType<typeof useThreadActions>["archiveThread"];
  deleteThread: ReturnType<typeof useThreadActions>["deleteThread"];
  threadJumpLabelByKey: ReadonlyMap<string, string>;
  attachThreadListAutoAnimateRef: (node: HTMLElement | null) => void;
  expandThreadListForProject: (projectKey: string) => void;
  collapseThreadListForProject: (projectKey: string) => void;
  dragInProgressRef: React.RefObject<boolean>;
  suppressProjectClickAfterDragRef: React.RefObject<boolean>;
  suppressProjectClickForContextMenuRef: React.RefObject<boolean>;
  isManualProjectSorting: boolean;
  dragHandleProps: SortableProjectHandleProps | null;
  environmentCard?: EnvironmentCardIdentity | null;
}

const SidebarProjectItem = memo(function SidebarProjectItem(props: SidebarProjectItemProps) {
  const {
    project,
    isThreadListExpanded,
    activeRouteThreadKey,
    moduleRouteActive,
    selectedProjectKey,
    selectProject,
    openCreateWorktreeDialog,
    archiveThread,
    deleteThread,
    threadJumpLabelByKey,
    attachThreadListAutoAnimateRef,
    expandThreadListForProject,
    collapseThreadListForProject,
    dragInProgressRef,
    suppressProjectClickAfterDragRef,
    suppressProjectClickForContextMenuRef,
    isManualProjectSorting,
    dragHandleProps,
    environmentCard = null,
  } = props;
  const pullRequestsEnabled = usePrimarySettings((settings) => settings.pullRequestsEnabled);
  const requestWorktreeRemoval = useContext(WorktreeRemovalRequestContext);
  const threadSortOrder = useClientSettings<SidebarThreadSortOrder>(
    (settings) => settings.sidebarThreadSortOrder,
  );
  const appSettingsConfirmThreadDelete = useClientSettings<boolean>(
    (settings) => settings.confirmThreadDelete,
  );
  const appSettingsConfirmThreadArchive = useClientSettings<boolean>(
    (settings) => settings.confirmThreadArchive,
  );
  const projectGroupingSettings = useClientSettings(selectProjectGroupingSettings);
  const serverConfigs = useServerConfigs();
  const deleteProject = useAtomCommand(projectEnvironment.delete, {
    reportFailure: false,
  });
  const updateProject = useAtomCommand(projectEnvironment.update, {
    reportFailure: false,
  });
  const updateWorktreeDiscoveryPolicy = useAtomCommand(worktreeEnvironment.updatePolicy, {
    reportFailure: false,
  });
  const updateThreadMetadata = useAtomCommand(threadEnvironment.updateMetadata, {
    reportFailure: false,
  });
  const createDefaultThread = useAtomCommand(threadEnvironment.create, {
    reportFailure: false,
  });
  const pinnedThreadKeys = useSidebarWorkspaceMetaStore((state) => state.pinnedThreadKeys);
  const unreadThreadKeys = useSidebarWorkspaceMetaStore((state) => state.unreadThreadKeys);
  const togglePinnedThreadKey = useSidebarWorkspaceMetaStore((state) => state.togglePinned);
  const markThreadRowUnread = useSidebarWorkspaceMetaStore((state) => state.markUnread);
  const markThreadRowRead = useSidebarWorkspaceMetaStore((state) => state.markRead);
  // "Update" runs vcs.pull for the row's
  // cwd (worktree path or project checkout), same atom-command pattern as
  // `useVcsPullAction` in sourceControlActions.ts -- that hook can't be used
  // here directly because its scope is fixed at mount, while the row clicked
  // varies per context-menu invocation.
  const pullWorkspaceRow = useAtomCommand(vcsEnvironment.pull, { reportFailure: false });
  const refreshVcsStatusAfterPull = useAtomCommand(vcsEnvironment.refreshStatus, {
    reportFailure: false,
  });
  const openInEditorMutation = useAtomCommand(shellEnvironment.openInEditor, {
    reportFailure: false,
  });
  const availableEditorsFor = useCallback(
    (environmentId: EnvironmentId): ReadonlyArray<EditorId> =>
      serverConfigs.get(environmentId)?.availableEditors ?? EMPTY_EDITOR_IDS,
    [serverConfigs],
  );
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const supportedWorktreeDiscoveryMembers = useMemo(
    () => getSupportedWorktreeDiscoveryMembers(project.memberProjects, serverConfigs),
    [project.memberProjects, serverConfigs],
  );
  const modelLabels = useMemo(() => buildWorkspaceModelLabels(serverConfigs), [serverConfigs]);
  const openWorkspaceInFileManager = useCallback(async (path: string) => {
    const bridge =
      typeof window === "undefined" ? undefined : window.desktopBridge?.openInFileManager;
    if (!bridge) return;
    try {
      await bridge(path, true);
    } catch (error) {
      toastManager.add({
        type: "error",
        title: "Unable to open File Explorer",
        description: error instanceof Error ? error.message : "An unexpected error occurred.",
      });
    }
  }, []);
  const updateSettings = useUpdateClientSettings();
  const sidebarThreadPreviewCount = useClientSettings<SidebarThreadPreviewCount>(
    (settings) => settings.sidebarThreadPreviewCount,
  );
  const router = useRouter();
  const navigate = useNavigate();
  const { isMobile, setOpenMobile } = useSidebar();
  const markThreadUnread = useUiStateStore((state) => state.markThreadUnread);
  const setProjectExpanded = useUiStateStore((state) => state.setProjectExpanded);
  const toggleThreadSelection = useThreadSelectionStore((state) => state.toggleThread);
  const rangeSelectTo = useThreadSelectionStore((state) => state.rangeSelectTo);
  const clearSelection = useThreadSelectionStore((state) => state.clearSelection);
  const removeFromSelection = useThreadSelectionStore((state) => state.removeFromSelection);
  const setSelectionAnchor = useThreadSelectionStore((state) => state.setAnchor);
  const { copyToClipboard: copyThreadIdToClipboard } = useCopyToClipboard<{
    threadId: ThreadId;
  }>({
    onCopy: (ctx) => {
      toastManager.add({
        type: "success",
        title: "Thread ID copied",
        description: ctx.threadId,
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Failed to copy thread ID",
          description: error instanceof Error ? error.message : "An error occurred.",
        }),
      );
    },
  });
  const { copyToClipboard: copyPathToClipboard } = useCopyToClipboard<{
    path: string;
  }>({
    onCopy: (ctx) => {
      toastManager.add({
        type: "success",
        title: "Path copied",
        description: ctx.path,
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Failed to copy path",
          description: error instanceof Error ? error.message : "An error occurred.",
        }),
      );
    },
  });
  const { copyToClipboard: copyBranchNameToClipboard } = useCopyToClipboard<{
    branch: string;
  }>({
    onCopy: (ctx) => {
      toastManager.add({
        type: "success",
        title: "Branch name copied",
        description: ctx.branch,
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Failed to copy branch name",
          description: error instanceof Error ? error.message : "An error occurred.",
        }),
      );
    },
  });
  const openPrLink = useOpenPrLink();
  const sidebarThreads = useThreadShellsForProjectRefs(project.memberProjectRefs);
  const sidebarThreadByKey = useMemo(
    () =>
      new Map(
        sidebarThreads.map(
          (thread) =>
            [scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)), thread] as const,
        ),
      ),
    [sidebarThreads],
  );
  // Keep a ref so callbacks can read the latest map without appearing in
  // dependency arrays (avoids invalidating every thread-row memo on each
  // thread-list change).
  const sidebarThreadByKeyRef = useRef(sidebarThreadByKey);
  useLayoutEffect(() => {
    sidebarThreadByKeyRef.current = sidebarThreadByKey;
  }, [sidebarThreadByKey]);
  const projectThreads = sidebarThreads;
  const projectPreferenceKeys = useMemo(() => projectExpansionPreferenceKeys(project), [project]);
  const projectToggleKeys = useMemo(() => projectExpansionToggleKeys(project), [project]);
  const projectExpanded = useUiStateStore((state) =>
    resolveProjectExpanded(state.projectExpandedById, projectPreferenceKeys),
  );
  const threadLastVisitedAts = useUiStateStore(
    useShallow((state) =>
      projectThreads.map(
        (thread) =>
          state.threadLastVisitedAtById[
            scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id))
          ] ?? null,
      ),
    ),
  );
  const [renamingThreadKey, setRenamingThreadKey] = useState<string | null>(null);
  const [renamingTitle, setRenamingTitle] = useState("");
  const [confirmingArchiveThreadKey, setConfirmingArchiveThreadKey] = useState<string | null>(null);
  const [projectRenameTarget, setProjectRenameTarget] = useState<SidebarProjectGroupMember | null>(
    null,
  );
  const [projectRenameTitle, setProjectRenameTitle] = useState("");
  const [importSessionsTarget, setImportSessionsTarget] = useState<ImportCliSessionsTarget | null>(
    null,
  );
  const [projectGroupingTarget, setProjectGroupingTarget] =
    useState<SidebarProjectGroupMember | null>(null);
  const [projectGroupingSelection, setProjectGroupingSelection] = useState<
    SidebarProjectGroupingMode | "inherit"
  >("inherit");
  const renamingCommittedRef = useRef(false);
  const renamingInputRef = useRef<HTMLInputElement | null>(null);
  const confirmArchiveButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  // Hidden discovered-worktree count reported by the mounted discovery section;
  // null while the section is unmounted (collapsed project) or not loaded. A ref,
  // because only the project menu reads it, when it opens.
  const discoveryHiddenCountRef = useRef<number | null>(null);
  const handleDiscoveryHiddenCountChange = useCallback((count: number | null) => {
    discoveryHiddenCountRef.current = count;
  }, []);
  const memberProjectByScopedKey = useMemo(
    () =>
      new Map(
        project.memberProjects.map((member) => [
          scopedProjectKey(scopeProjectRef(member.environmentId, member.id)),
          member,
        ]),
      ),
    [project.memberProjects],
  );
  const memberThreadCountByPhysicalKey = useMemo(() => {
    const counts = new Map<string, number>(
      project.memberProjects.map((member) => [member.physicalProjectKey, 0] as const),
    );
    for (const thread of projectThreads) {
      const member = memberProjectByScopedKey.get(
        scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId)),
      );
      if (!member) {
        continue;
      }
      counts.set(member.physicalProjectKey, (counts.get(member.physicalProjectKey) ?? 0) + 1);
    }
    return counts;
  }, [memberProjectByScopedKey, project.memberProjects, projectThreads]);

  const workspaceStatuses = useMemo(
    () => createWorkspaceStatusLookup(projectThreads, threadLastVisitedAts),
    [projectThreads, threadLastVisitedAts],
  );
  const {
    chatSummaries,
    primaryThread,
    projectStatus,
    visibleProjectThreads,
    orderedProjectThreadKeys,
  } = useMemo(() => {
    // Workspace-row model: the project's `kind: "default"`
    // thread is the primary row (rendered separately, see
    // `SidebarPrimaryRow`) — everything else is a worktree/ad-hoc
    // workspace row, ordered pinned-first (sidebarWorkspaceMetaStore) then
    // by the existing sortThreads order.
    const { primaryThread, workspaceThreads } = splitPrimaryAndWorkspaceThreads(
      projectThreads.filter((thread) => thread.archivedAt === null),
    );
    const visibleProjectThreads = orderRowsWithPins(
      sortThreads(workspaceThreads, threadSortOrder),
      pinnedThreadKeys,
      (thread) => scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
    );
    const projectStatus = resolveHighestWorkspaceCardStatus(
      [...(primaryThread ? [primaryThread] : []), ...visibleProjectThreads].map(
        workspaceStatuses.statusOf,
      ),
    );
    const chatSummaries = summarizeWorkspaceChats(projectThreads, workspaceStatuses.lastVisitedAt);
    return {
      chatSummaries,
      orderedProjectThreadKeys: visibleProjectThreads.map((thread) =>
        scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
      ),
      primaryThread,
      projectStatus,
      visibleProjectThreads,
    };
  }, [pinnedThreadKeys, projectThreads, threadSortOrder, workspaceStatuses]);
  const pinnedCollapsedThread = useMemo(() => {
    const activeThreadKey = activeRouteThreadKey ?? undefined;
    if (!activeThreadKey || projectExpanded) {
      return null;
    }
    return (
      visibleProjectThreads.find(
        (thread) =>
          scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)) === activeThreadKey,
      ) ?? null
    );
  }, [activeRouteThreadKey, projectExpanded, visibleProjectThreads]);
  // The primary row's thread was
  // split out of `visibleProjectThreads`, so `pinnedCollapsedThread` (which
  // only searches that array) never matched it — collapsing a project while
  // routed to its own default thread hid the primary row entirely, unlike
  // regular threads which peek through when collapsed+active. Tracked
  // separately (not folded into `pinnedCollapsedThread` itself) because that
  // value also drives which threads `SidebarProjectThreadList` renders, and
  // the primary thread must stay rendered via `SidebarPrimaryRow`, not
  // `SidebarThreadRow`.
  const isPrimaryThreadActiveWhileCollapsed = useMemo(() => {
    if (!primaryThread || projectExpanded || !activeRouteThreadKey) {
      return false;
    }
    return (
      scopedThreadKey(scopeThreadRef(primaryThread.environmentId, primaryThread.id)) ===
      activeRouteThreadKey
    );
  }, [activeRouteThreadKey, primaryThread, projectExpanded]);

  const { hasOverflowingThreads, hiddenThreadStatus, renderedThreads, shouldShowThreadPanel } =
    useMemo(() => {
      const hasOverflowingThreads = visibleProjectThreads.length > sidebarThreadPreviewCount;
      const previewThreads =
        isThreadListExpanded || !hasOverflowingThreads
          ? visibleProjectThreads
          : visibleProjectThreads.slice(0, sidebarThreadPreviewCount);
      const visibleThreadKeys = new Set(
        [...previewThreads, ...(pinnedCollapsedThread ? [pinnedCollapsedThread] : [])].map(
          (thread) => scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
        ),
      );
      // Collapsed + routed to the primary (default) thread: `pinnedCollapsedThread`
      // stays null (it only searches workspace threads, see
      // `isPrimaryThreadActiveWhileCollapsed` above) and `shouldShowThreadPanel`
      // is true so the primary row can peek through -- but that must NOT fall
      // into the `previewThreads` branch below, or every workspace row's
      // preview slice would render alongside it. Only render workspace rows
      // here when actually expanded or peeking a specific pinned/active one.
      const renderedThreads = pinnedCollapsedThread
        ? [pinnedCollapsedThread]
        : projectExpanded
          ? visibleProjectThreads.filter((thread) =>
              visibleThreadKeys.has(
                scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
              ),
            )
          : [];
      const hiddenThreads = visibleProjectThreads.filter(
        (thread) =>
          !visibleThreadKeys.has(scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id))),
      );
      return {
        hasOverflowingThreads,
        hiddenThreadStatus: resolveHighestWorkspaceCardStatus(
          hiddenThreads.map(workspaceStatuses.statusOf),
        ),
        renderedThreads,
        shouldShowThreadPanel:
          projectExpanded || pinnedCollapsedThread !== null || isPrimaryThreadActiveWhileCollapsed,
      };
    }, [
      isPrimaryThreadActiveWhileCollapsed,
      isThreadListExpanded,
      pinnedCollapsedThread,
      projectExpanded,
      sidebarThreadPreviewCount,
      visibleProjectThreads,
      workspaceStatuses,
    ]);

  const projectSelected = selectedProjectKey === project.projectKey;

  const handleProjectButtonClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      if (suppressProjectClickForContextMenuRef.current) {
        suppressProjectClickForContextMenuRef.current = false;
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (dragInProgressRef.current) {
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (suppressProjectClickAfterDragRef.current) {
        suppressProjectClickAfterDragRef.current = false;
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (useThreadSelectionStore.getState().hasSelection()) {
        clearSelection();
      }
      // The clicked header becomes the selected node; expansion still toggles.
      selectProject(project.projectKey);
      setProjectExpanded(projectToggleKeys, !projectExpanded);
    },
    [
      clearSelection,
      dragInProgressRef,
      project.projectKey,
      projectExpanded,
      projectToggleKeys,
      selectProject,
      setProjectExpanded,
      suppressProjectClickAfterDragRef,
      suppressProjectClickForContextMenuRef,
    ],
  );

  const handleProjectButtonKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLButtonElement>) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      if (dragInProgressRef.current) {
        return;
      }
      selectProject(project.projectKey);
      setProjectExpanded(projectToggleKeys, !projectExpanded);
    },
    [
      dragInProgressRef,
      project.projectKey,
      projectExpanded,
      projectToggleKeys,
      selectProject,
      setProjectExpanded,
    ],
  );

  const handleProjectButtonPointerDownCapture = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      suppressProjectClickForContextMenuRef.current = false;
      if (
        isContextMenuPointerDown({
          button: event.button,
          ctrlKey: event.ctrlKey,
          isMac: isMacPlatform(navigator.platform),
        })
      ) {
        event.stopPropagation();
      }

      suppressProjectClickAfterDragRef.current = false;
    },
    [suppressProjectClickAfterDragRef, suppressProjectClickForContextMenuRef],
  );

  const openProjectRenameDialog = useCallback((member: SidebarProjectGroupMember) => {
    setProjectRenameTarget(member);
    setProjectRenameTitle(member.title);
  }, []);

  const openProjectGroupingDialog = useCallback(
    (member: SidebarProjectGroupMember) => {
      const overrideKey = deriveProjectGroupingOverrideKey(member);
      setProjectGroupingTarget(member);
      setProjectGroupingSelection(
        projectGroupingSettings.sidebarProjectGroupingOverrides?.[overrideKey] ?? "inherit",
      );
    },
    [projectGroupingSettings.sidebarProjectGroupingOverrides],
  );

  const removeProject = useCallback(
    async (member: SidebarProjectGroupMember, options: { force?: boolean } = {}) => {
      const memberProjectRef = scopeProjectRef(member.environmentId, member.id);
      const result = await deleteProject({
        environmentId: member.environmentId,
        input: {
          projectId: member.id,
          ...(options.force === true ? { force: true } : {}),
        },
      });
      if (result._tag === "Failure") {
        return result;
      }
      const draftStore = useComposerDraftStore.getState();
      const projectDraftThread = draftStore.getDraftThreadByProjectRef(memberProjectRef);
      if (projectDraftThread) {
        draftStore.clearDraftThread(projectDraftThread.draftId);
      }
      draftStore.clearProjectDraftThreadId(memberProjectRef);
      return result;
    },
    [deleteProject],
  );

  const handleRemoveProject = useCallback(
    async (member: SidebarProjectGroupMember) => {
      const api = readLocalApi();
      if (!api) {
        return;
      }

      const memberProjectRef = scopeProjectRef(member.environmentId, member.id);
      const memberThreadCount = memberThreadCountByPhysicalKey.get(member.physicalProjectKey) ?? 0;
      if (memberThreadCount > 0) {
        const warningToastId = toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: "Project is not empty",
            description: "Delete all threads in this project before removing it.",
            actionVariant: "destructive",
            actionProps: {
              children: "Delete anyway",
              onClick: () => {
                void (async () => {
                  toastManager.close(warningToastId);
                  await new Promise<void>((resolve) => {
                    window.setTimeout(resolve, 180);
                  });

                  const latestProjectThreads = Array.from(
                    sidebarThreadByKeyRef.current.values(),
                  ).filter(
                    (thread) =>
                      thread.environmentId === memberProjectRef.environmentId &&
                      thread.projectId === memberProjectRef.projectId,
                  );
                  const confirmed = await api.dialogs.confirm(
                    latestProjectThreads.length > 0
                      ? [
                          `Remove project "${member.title}" and delete its ${latestProjectThreads.length} thread${
                            latestProjectThreads.length === 1 ? "" : "s"
                          }?`,
                          `Path: ${member.workspaceRoot}`,
                          ...(member.environmentLabel
                            ? [`Environment: ${member.environmentLabel}`]
                            : []),
                          "This permanently clears conversation history for those threads.",
                          "This removes only this project entry.",
                          "This action cannot be undone.",
                        ].join("\n")
                      : [
                          `Remove project "${member.title}"?`,
                          `Path: ${member.workspaceRoot}`,
                          ...(member.environmentLabel
                            ? [`Environment: ${member.environmentLabel}`]
                            : []),
                          "This removes only this project entry.",
                        ].join("\n"),
                  );
                  if (!confirmed) {
                    return;
                  }

                  const result = await removeProject(member, { force: true });
                  if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
                    const error = squashAtomCommandFailure(result);
                    toastManager.add(
                      stackedThreadToast({
                        type: "error",
                        title: `Failed to remove "${member.title}"`,
                        description:
                          error instanceof Error
                            ? error.message
                            : "Unknown error removing project.",
                      }),
                    );
                  }
                })().catch((error) => {
                  const message =
                    error instanceof Error ? error.message : "Unknown error removing project.";
                  console.error("Failed to remove project", {
                    projectId: member.id,
                    environmentId: member.environmentId,
                    ...safeErrorLogAttributes(error),
                  });
                  toastManager.add(
                    stackedThreadToast({
                      type: "error",
                      title: `Failed to remove "${member.title}"`,
                      description: message,
                    }),
                  );
                });
              },
            },
          }),
        );
        return;
      }

      const message = [
        `Remove project "${member.title}"?`,
        `Path: ${member.workspaceRoot}`,
        ...(member.environmentLabel ? [`Environment: ${member.environmentLabel}`] : []),
        "This removes only this project entry.",
      ].join("\n");
      const confirmed = await api.dialogs.confirm(message);
      if (!confirmed) {
        return;
      }

      const result = await removeProject(member);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        const message = error instanceof Error ? error.message : "Unknown error removing project.";
        console.error("Failed to remove project", {
          projectId: member.id,
          environmentId: member.environmentId,
          ...safeErrorLogAttributes(error),
        });
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: `Failed to remove "${member.title}"`,
            description: message,
          }),
        );
      }
    },
    [memberThreadCountByPhysicalKey, removeProject],
  );

  const navigateToThread = useCallback(
    (threadRef: ScopedThreadRef) => {
      if (useThreadSelectionStore.getState().selectedThreadKeys.size > 0) {
        clearSelection();
      }
      setSelectionAnchor(scopedThreadKey(threadRef));
      if (isMobile) {
        setOpenMobile(false);
      }
      void router.navigate({
        to: "/$environmentId/$threadId",
        params: buildThreadRouteParams(threadRef),
      });
    },
    [clearSelection, isMobile, router, setOpenMobile, setSelectionAnchor],
  );

  const handlePrimaryRowClick = useCallback(() => {
    if (primaryThread) {
      navigateToThread(scopeThreadRef(primaryThread.environmentId, primaryThread.id));
      return;
    }
    // Server backfill (pinned interface 1) may not have run yet for this
    // project — create the default thread on demand, then navigate.
    const threadId = newThreadId();
    // This fallback-provider selection is a client-side
    // safety net for the rare pre-backfill case; revisit once every project
    // is guaranteed a default thread server-side.
    const serverConfig = serverConfigs.get(project.environmentId);
    const settings = serverConfig?.settings ?? DEFAULT_SERVER_SETTINGS;
    const fallbackProvider = serverConfig?.providers.find((provider) => provider.enabled);
    const targetInstanceId =
      project.defaultModelSelection?.instanceId ??
      fallbackProvider?.instanceId ??
      ProviderInstanceId.make("codex");
    const resolution = resolveProviderSessionSelectionForInstance({
      instanceId: targetInstanceId,
      providers: serverConfig?.providers ?? [],
      settings,
      projectSelection: project.defaultModelSelection,
    });
    if (resolution.fallback) {
      console.warn("Provider session default fallback", resolution.fallback);
    }
    void (async () => {
      const result = await createDefaultThread({
        environmentId: project.environmentId,
        input: {
          threadId,
          projectId: project.id,
          title: project.displayName,
          modelSelection: resolution.modelSelection,
          runtimeMode: DEFAULT_RUNTIME_MODE,
          interactionMode: DEFAULT_PROVIDER_INTERACTION_MODE,
          branch: null,
        },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        return;
      }
      navigateToThread(scopeThreadRef(project.environmentId, threadId));
    })();
  }, [createDefaultThread, navigateToThread, primaryThread, project, serverConfigs]);

  const handleThreadClick = useCallback(
    (
      event: React.MouseEvent,
      threadRef: ScopedThreadRef,
      orderedProjectThreadKeys: readonly string[],
    ) => {
      const isMac = isMacPlatform(navigator.platform);
      const isModClick = isMac ? event.metaKey : event.ctrlKey;
      const isShiftClick = event.shiftKey;
      const threadKey = scopedThreadKey(threadRef);
      const currentSelectionCount = useThreadSelectionStore.getState().selectedThreadKeys.size;

      if (isModClick) {
        event.preventDefault();
        toggleThreadSelection(threadKey);
        return;
      }

      if (isShiftClick) {
        event.preventDefault();
        rangeSelectTo(threadKey, orderedProjectThreadKeys);
        return;
      }

      // Ignore the trailing click of a plain double-click so it doesn't navigate
      // while a double-click is starting an inline rename. Placed after the
      // modifier branches so cmd/shift selection still processes every click.
      if (isTrailingDoubleClick(event.detail)) {
        return;
      }

      if (currentSelectionCount > 0) {
        clearSelection();
      }
      setSelectionAnchor(threadKey);
      if (isMobile) {
        setOpenMobile(false);
      }
      void router.navigate({
        to: "/$environmentId/$threadId",
        params: buildThreadRouteParams(threadRef),
      });
    },
    [
      clearSelection,
      isMobile,
      rangeSelectTo,
      router,
      setOpenMobile,
      setSelectionAnchor,
      toggleThreadSelection,
    ],
  );

  const handleMultiSelectContextMenu = useCallback(
    async (position: { x: number; y: number }) => {
      const api = readLocalApi();
      if (!api) return;
      const threadKeys = [...useThreadSelectionStore.getState().selectedThreadKeys];
      if (threadKeys.length === 0) return;
      const count = threadKeys.length;

      const clicked = await api.contextMenu.show(buildMultiSelectMenu(count), position);

      if (clicked === "mark-unread") {
        for (const threadKey of threadKeys) {
          const thread = sidebarThreadByKeyRef.current.get(threadKey);
          markThreadUnread(threadKey, thread?.latestTurn?.completedAt);
        }
        clearSelection();
        return;
      }

      if (clicked !== "delete") return;

      if (appSettingsConfirmThreadDelete) {
        const worktreeCount = threadKeys.reduce((count, threadKey) => {
          const thread = sidebarThreadByKeyRef.current.get(threadKey);
          return count + (thread?.worktreePath ? 1 : 0);
        }, 0);
        const confirmed = await api.dialogs.confirm(
          getBulkThreadDeletionConfirmation(count, worktreeCount),
        );
        if (!confirmed) return;
      }

      const deletedThreadKeys = new Set(threadKeys);
      for (const threadKey of threadKeys) {
        const thread = sidebarThreadByKeyRef.current.get(threadKey);
        if (!thread) continue;
        const result = await deleteThread(scopeThreadRef(thread.environmentId, thread.id), {
          deletedThreadKeys,
        });
        if (result._tag === "Failure") {
          if (!isAtomCommandInterrupted(result)) {
            const error = squashAtomCommandFailure(result);
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Failed to delete threads",
                description: error instanceof Error ? error.message : "An error occurred.",
              }),
            );
          }
          return;
        }
      }
      removeFromSelection(threadKeys);
    },
    [
      appSettingsConfirmThreadDelete,
      clearSelection,
      deleteThread,
      markThreadUnread,
      removeFromSelection,
    ],
  );

  const runProjectMemberAction = useCallback(
    (
      event: React.MouseEvent<HTMLButtonElement>,
      action: (member: SidebarProjectGroupMember) => void,
    ) => {
      event.preventDefault();
      event.stopPropagation();

      if (project.memberProjects.length === 1) {
        action(project.memberProjects[0]!);
        return;
      }

      void (async () => {
        const member = await chooseProjectMember(project.memberProjects, {
          x: event.clientX,
          y: event.clientY,
        });
        if (member) action(member);
      })();
    },
    [project.memberProjects],
  );

  const openWorktreeForProjectMember = useCallback(
    (member: SidebarProjectGroupMember) => {
      if (isMobile) setOpenMobile(false);
      openCreateWorktreeDialog(scopeProjectRef(member.environmentId, member.id));
    },
    [isMobile, openCreateWorktreeDialog, setOpenMobile],
  );

  const handleCreateWorktreeClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      runProjectMemberAction(event, openWorktreeForProjectMember);
    },
    [openWorktreeForProjectMember, runProjectMemberAction],
  );

  const openProjectHeaderMenu = useCallback(
    async (position: { x: number; y: number }) => {
      const api = readLocalApi();
      if (!api) return;
      const discoveryVisibility = supportedWorktreeDiscoveryMembers.some(
        (member) => member.worktreeDiscovery.visibility === "shown",
      )
        ? "shown"
        : "hidden";
      const clicked = await api.contextMenu.show(
        buildProjectHeaderMenu({
          members: project.memberProjects.map((member) => ({
            physicalProjectKey: member.physicalProjectKey,
            label: formatProjectMemberActionLabel(member, project.groupedProjectCount),
          })),
          discovery:
            supportedWorktreeDiscoveryMembers.length > 0
              ? {
                  visibility: discoveryVisibility,
                  // Discovery is mounted only while expanded, so only then is the count known.
                  hiddenCount: projectExpanded ? discoveryHiddenCountRef.current : null,
                }
              : null,
        }),
        position,
      );
      if (!clicked) return;

      if (clicked === "archive") {
        setOpenMobile(false);
        await router.navigate({ to: "/settings/archived" });
        return;
      }

      if (clicked === "worktree-discovery-visibility") {
        const nextVisibility = discoveryVisibility === "hidden" ? "shown" : "hidden";
        const results = await Promise.all(
          supportedWorktreeDiscoveryMembers.map((member) =>
            updateWorktreeDiscoveryPolicy({
              environmentId: member.environmentId,
              input: {
                commandId: newCommandId(),
                projectId: member.id,
                visibility: nextVisibility,
              },
            }),
          ),
        );
        for (const [index, result] of results.entries()) {
          if (result._tag !== "Failure" || isAtomCommandInterrupted(result)) continue;
          const member = supportedWorktreeDiscoveryMembers[index]!;
          const error = squashAtomCommandFailure(result);
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: `Could not update worktree visibility for ${member.environmentLabel ?? member.title}`,
              description: error instanceof Error ? error.message : "An unexpected error occurred.",
            }),
          );
        }
        return;
      }

      const selection = parseProjectHeaderSelection(clicked);
      const member = selection
        ? project.memberProjects.find(
            (candidate) => candidate.physicalProjectKey === selection.physicalProjectKey,
          )
        : undefined;
      if (!selection || !member) return;
      switch (selection.action) {
        case "new-worktree":
          openWorktreeForProjectMember(member);
          return;
        case "import-sessions":
          setImportSessionsTarget({
            environmentId: member.environmentId,
            projectId: member.id,
            workspaceRoot: member.workspaceRoot,
          });
          return;
        case "rename":
          openProjectRenameDialog(member);
          return;
        case "grouping":
          openProjectGroupingDialog(member);
          return;
        case "copy-path":
          copyPathToClipboard(member.workspaceRoot, { path: member.workspaceRoot });
          return;
        case "delete":
          await handleRemoveProject(member);
          return;
      }
    },
    [
      copyPathToClipboard,
      handleRemoveProject,
      openProjectGroupingDialog,
      openProjectRenameDialog,
      openWorktreeForProjectMember,
      project.groupedProjectCount,
      project.memberProjects,
      projectExpanded,
      router,
      setOpenMobile,
      supportedWorktreeDiscoveryMembers,
      updateWorktreeDiscoveryPolicy,
    ],
  );

  const handleProjectButtonContextMenu = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      suppressProjectClickForContextMenuRef.current = true;
      void openProjectHeaderMenu({ x: event.clientX, y: event.clientY });
    },
    [openProjectHeaderMenu, suppressProjectClickForContextMenuRef],
  );

  // ⋯ is a real button, so Enter and Space open it too; the menu appears below it.
  const handleProjectActionsClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      void openProjectHeaderMenu(
        contextMenuAnchorForRect(event.currentTarget.getBoundingClientRect()),
      );
    },
    [openProjectHeaderMenu],
  );

  const openGitManagerForProjectMember = useCallback(
    (member: SidebarProjectGroupMember) => {
      if (isMobile) setOpenMobile(false);
      void navigate({
        to: "/project/$environmentId/$projectId/git",
        params: { environmentId: member.environmentId, projectId: member.id },
      });
    },
    [isMobile, navigate, setOpenMobile],
  );

  const handleOpenGitManagerClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      runProjectMemberAction(event, openGitManagerForProjectMember);
    },
    [openGitManagerForProjectMember, runProjectMemberAction],
  );

  const openPullRequestsForProjectMember = useCallback(
    (member: SidebarProjectGroupMember) => {
      if (isMobile) setOpenMobile(false);
      void navigate({
        to: "/project/$environmentId/$projectId/pull-requests",
        params: { environmentId: member.environmentId, projectId: member.id },
      });
    },
    [isMobile, navigate, setOpenMobile],
  );

  const handleOpenPullRequestsClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      runProjectMemberAction(event, openPullRequestsForProjectMember);
    },
    [openPullRequestsForProjectMember, runProjectMemberAction],
  );

  const attemptArchiveThread = useCallback(
    async (threadRef: ScopedThreadRef) => {
      const result = await archiveThread(threadRef);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Failed to archive thread",
            description: error instanceof Error ? error.message : "An error occurred.",
          }),
        );
      }
    },
    [archiveThread],
  );

  const cancelRename = useCallback(() => {
    setRenamingThreadKey(null);
    renamingInputRef.current = null;
  }, []);

  const startThreadRename = useCallback((threadKey: string, title: string) => {
    setRenamingThreadKey(threadKey);
    setRenamingTitle(title);
    renamingCommittedRef.current = false;
  }, []);

  const commitRename = useCallback(
    async (threadRef: ScopedThreadRef, newTitle: string, originalTitle: string) => {
      const threadKey = scopedThreadKey(threadRef);
      const finishRename = () => {
        setRenamingThreadKey((current) => {
          if (current !== threadKey) return current;
          renamingInputRef.current = null;
          return null;
        });
      };

      const trimmed = newTitle.trim();
      if (trimmed.length === 0) {
        toastManager.add({
          type: "warning",
          title: "Thread title cannot be empty",
        });
        finishRename();
        return;
      }
      if (trimmed === originalTitle) {
        finishRename();
        return;
      }
      const result = await updateThreadMetadata({
        environmentId: threadRef.environmentId,
        input: {
          threadId: threadRef.threadId,
          title: trimmed,
        },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Failed to rename thread",
            description: error instanceof Error ? error.message : "An error occurred.",
          }),
        );
      }
      finishRename();
    },
    [updateThreadMetadata],
  );

  const closeProjectRenameDialog = useCallback(() => {
    setProjectRenameTarget(null);
    setProjectRenameTitle("");
  }, []);

  const handleImportSessionsOpenChange = useCallback((open: boolean) => {
    if (!open) setImportSessionsTarget(null);
  }, []);

  const openImportedThread = useCallback(
    (target: ImportCliSessionsTarget, threadId: ThreadId) => {
      setImportSessionsTarget(null);
      navigateToThread(scopeThreadRef(target.environmentId, threadId));
    },
    [navigateToThread],
  );

  const handleSessionsImported = useCallback(
    (target: ImportCliSessionsTarget, summary: ImportCliSessionsSummary) => {
      setImportSessionsTarget(null);
      toastManager.add(
        stackedThreadToast({
          type: summary.type,
          title: summary.title,
          ...(summary.description === undefined ? {} : { description: summary.description }),
        }),
      );
      if (summary.newestThreadId === null) return;
      openImportedThread(target, summary.newestThreadId);
    },
    [openImportedThread],
  );

  const submitProjectRename = useCallback(async () => {
    if (!projectRenameTarget) {
      return;
    }

    const trimmed = projectRenameTitle.trim();
    if (trimmed.length === 0) {
      toastManager.add({
        type: "warning",
        title: "Project title cannot be empty",
      });
      return;
    }

    if (trimmed === projectRenameTarget.title) {
      closeProjectRenameDialog();
      return;
    }

    const result = await updateProject({
      environmentId: projectRenameTarget.environmentId,
      input: {
        projectId: projectRenameTarget.id,
        title: trimmed,
      },
    });
    if (result._tag === "Success") {
      closeProjectRenameDialog();
    } else if (!isAtomCommandInterrupted(result)) {
      const error = squashAtomCommandFailure(result);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Failed to rename project",
          description: error instanceof Error ? error.message : "An error occurred.",
        }),
      );
    }
  }, [closeProjectRenameDialog, projectRenameTarget, projectRenameTitle, updateProject]);

  const closeProjectGroupingDialog = useCallback(() => {
    setProjectGroupingTarget(null);
    setProjectGroupingSelection("inherit");
  }, []);

  const saveProjectGroupingPreference = useCallback(() => {
    if (!projectGroupingTarget) {
      return;
    }

    const overrideKey = deriveProjectGroupingOverrideKey(projectGroupingTarget);
    const nextOverrides = {
      ...projectGroupingSettings.sidebarProjectGroupingOverrides,
    };
    if (projectGroupingSelection === "inherit") {
      delete nextOverrides[overrideKey];
    } else {
      nextOverrides[overrideKey] = projectGroupingSelection;
    }
    updateSettings({
      sidebarProjectGroupingOverrides: nextOverrides,
    });
    closeProjectGroupingDialog();
  }, [
    closeProjectGroupingDialog,
    projectGroupingSelection,
    projectGroupingSettings.sidebarProjectGroupingOverrides,
    projectGroupingTarget,
    updateSettings,
  ]);

  const handleThreadContextMenu = useCallback(
    async (
      threadRef: ScopedThreadRef,
      position: { x: number; y: number },
      worktreeStatus: VcsAdoptedWorktreeStatus | null,
      branchName: string | null,
    ) => {
      const api = readLocalApi();
      if (!api) return;
      const threadKey = scopedThreadKey(threadRef);
      const thread = sidebarThreadByKeyRef.current.get(threadKey) ?? null;
      if (!thread) return;
      const threadProject = memberProjectByScopedKey.get(
        scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId)),
      );
      const threadWorkspacePath =
        thread.worktreePath ?? threadProject?.workspaceRoot ?? project.workspaceRoot ?? null;
      const workspaceActionsAvailable = selectWorktreeWorkspaceActionsAvailable(worktreeStatus);
      const threadServerConfig = serverConfigs.get(thread.environmentId);
      const removalPolicy =
        threadServerConfig === undefined
          ? null
          : selectWorktreeCatalogCapabilityPolicy(threadServerConfig.environment).removal;
      // The "Open in" submenu is built from the same EDITORS
      // list + availableEditors filter as OpenInPicker.tsx, adapted to the
      // native-style {id,label,children} shape `api.contextMenu.show` expects
      // (that component renders its own React menu, so it can't be reused
      // as-is here).
      const openInEditorOptions = EDITORS.filter((editor) =>
        availableEditorsFor(thread.environmentId).includes(editor.id),
      );
      const canOpenInFileExplorer =
        thread.environmentId === primaryEnvironmentId && typeof window !== "undefined"
          ? window.desktopBridge?.openInFileManager !== undefined
          : false;
      const openInChildren = workspaceActionsAvailable
        ? [
            ...(canOpenInFileExplorer
              ? [{ id: "open-in:file-explorer", label: "File Explorer" }]
              : []),
            ...openInEditorOptions.map((editor) => ({
              id: `open-in:${editor.id}`,
              label: editor.label,
            })),
          ]
        : [];
      const clicked = await api.contextMenu.show(
        buildWorkspaceCardMenu({
          isWorktree: thread.worktreePath !== null,
          openIn: openInChildren,
          pullDisabledReason: !threadWorkspacePath ? "This thread has no workspace path." : null,
          workspaceUnavailableReason:
            worktreeStatus &&
            worktreeStatus.availability !== "present" &&
            worktreeStatus.availability !== "verification-unavailable"
              ? describeUnavailableWorkspace(worktreeStatus.availability)
              : null,
          branchName,
          pinned: pinnedThreadKeys.includes(threadKey),
          unread: unreadThreadKeys.includes(threadKey),
          confirmThreadDelete: appSettingsConfirmThreadDelete,
          worktreeSessionRunning: isWorktreeSessionRunning(
            thread,
            sidebarThreadByKeyRef.current.values(),
          ),
        }),
        position,
      );

      if (clicked === "pull") {
        if (!threadWorkspacePath || !workspaceActionsAvailable) return;
        const pullResult = await pullWorkspaceRow({
          environmentId: thread.environmentId,
          input: { cwd: threadWorkspacePath },
        });
        if (pullResult._tag === "Failure") {
          if (!isAtomCommandInterrupted(pullResult)) {
            const error = squashAtomCommandFailure(pullResult);
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Failed to pull",
                description: error instanceof Error ? error.message : "An error occurred.",
              }),
            );
          }
          return;
        }
        await refreshVcsStatusAfterPull({
          environmentId: thread.environmentId,
          input: { cwd: threadWorkspacePath },
        });
        return;
      }

      if (clicked === "open-in:file-explorer") {
        if (!threadWorkspacePath) return;
        await openWorkspaceInFileManager(threadWorkspacePath);
        return;
      }

      if (typeof clicked === "string" && clicked.startsWith("open-in:")) {
        if (!threadWorkspacePath) return;
        const editor = openInEditorOptions.find(
          (candidate) => `open-in:${candidate.id}` === clicked,
        );
        if (!editor) return;
        const openResult = await openInEditorMutation({
          environmentId: thread.environmentId,
          input: { cwd: threadWorkspacePath, editor: editor.id },
        });
        if (openResult._tag === "Failure" && !isAtomCommandInterrupted(openResult)) {
          const error = squashAtomCommandFailure(openResult);
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Failed to open editor",
              description: error instanceof Error ? error.message : "An error occurred.",
            }),
          );
        }
        return;
      }

      if (clicked === "rename") {
        startThreadRename(threadKey, thread.title);
        return;
      }

      if (clicked === "mark-unread") {
        markThreadRowUnread(threadKey);
        return;
      }
      if (clicked === "mark-read") {
        markThreadRowRead(threadKey);
        return;
      }
      if (clicked === "toggle-pin") {
        togglePinnedThreadKey(threadKey);
        return;
      }
      if (clicked === "copy-path") {
        if (!threadWorkspacePath) {
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Path unavailable",
              description: "This thread does not have a workspace path to copy.",
            }),
          );
          return;
        }
        copyPathToClipboard(threadWorkspacePath, { path: threadWorkspacePath });
        return;
      }
      if (clicked === "copy-branch-name") {
        if (branchName !== null) {
          copyBranchNameToClipboard(branchName, { branch: branchName });
        }
        return;
      }
      if (clicked === "copy-thread-id") {
        copyThreadIdToClipboard(thread.id, { threadId: thread.id });
        return;
      }
      if (clicked !== "delete") return;
      if (thread.worktreePath) {
        // Re-check when chosen: the menu was built when it opened, and a session
        // may have started since. This is the card menu's Archive rule,
        // `isWorkspaceThreadRunning`, stricter than `archiveThread`'s turn check.
        const latestThreads = sidebarThreadByKeyRef.current;
        if (
          isWorktreeSessionRunning(latestThreads.get(threadKey) ?? thread, latestThreads.values())
        ) {
          toastManager.add(
            stackedThreadToast({
              type: "warning",
              title: "Worktree not deleted",
              description: WORKTREE_DELETE_BLOCKED_REASON,
            }),
          );
          return;
        }
        if (removalPolicy === "legacy-detach-only") {
          const confirmed = await api.dialogs.confirm(
            [
              `Remove worktree "${thread.title}" from BiBCode?`,
              "The Git worktree and its files will be left untouched.",
            ].join("\n"),
          );
          if (!confirmed) return;
          const result = await deleteThread(threadRef);
          if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
            const error = squashAtomCommandFailure(result);
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Failed to remove worktree from BiBCode",
                description: error instanceof Error ? error.message : "An error occurred.",
              }),
            );
          }
          return;
        }
        requestWorktreeRemoval?.({
          environmentId: thread.environmentId,
          projectId: thread.projectId,
          threadId: thread.id,
          title: thread.title,
          path: worktreeStatus?.path ?? thread.worktreePath,
          branch: worktreeStatus?.branch ?? thread.branch,
          availability: worktreeStatus?.availability ?? "verification-unavailable",
          registrationState: worktreeStatus?.registrationState ?? null,
          locked: worktreeStatus?.locked ?? false,
          ...(worktreeStatus?.lockReason ? { lockReason: worktreeStatus.lockReason } : {}),
        });
        return;
      }
      if (appSettingsConfirmThreadDelete) {
        const confirmed = await api.dialogs.confirm(
          [
            `Delete thread "${thread.title}"?`,
            "This permanently clears conversation history for this thread.",
          ].join("\n"),
        );
        if (!confirmed) {
          return;
        }
      }
      const result = await deleteThread(threadRef);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Failed to delete thread",
            description: error instanceof Error ? error.message : "An error occurred.",
          }),
        );
      }
    },
    [
      appSettingsConfirmThreadDelete,
      availableEditorsFor,
      copyBranchNameToClipboard,
      copyPathToClipboard,
      copyThreadIdToClipboard,
      deleteThread,
      markThreadRowRead,
      markThreadRowUnread,
      memberProjectByScopedKey,
      openInEditorMutation,
      openWorkspaceInFileManager,
      pinnedThreadKeys,
      primaryEnvironmentId,
      project.workspaceRoot,
      pullWorkspaceRow,
      refreshVcsStatusAfterPull,
      requestWorktreeRemoval,
      startThreadRename,
      togglePinnedThreadKey,
      unreadThreadKeys,
      serverConfigs,
    ],
  );

  // Menu for the primary (project checkout) card. The primary checkout is
  // read-only in the project tree, so project removal stays on the project
  // header menu instead of this card.
  const openPrimaryCardMenu = useCallback(
    (position: { x: number; y: number }, branchName: string | null) => {
      void (async () => {
        const api = readLocalApi();
        if (!api) return;
        const cwd = project.workspaceRoot;
        const primaryThreadKey = primaryThread
          ? scopedThreadKey(scopeThreadRef(primaryThread.environmentId, primaryThread.id))
          : null;
        const openInEditorOptions = EDITORS.filter((editor) =>
          availableEditorsFor(project.environmentId).includes(editor.id),
        );
        const canOpenInFileExplorer =
          project.environmentId === primaryEnvironmentId && typeof window !== "undefined"
            ? window.desktopBridge?.openInFileManager !== undefined
            : false;
        const openInChildren = [
          ...(canOpenInFileExplorer
            ? [{ id: "open-in:file-explorer", label: "File Explorer" }]
            : []),
          ...openInEditorOptions.map((editor) => ({
            id: `open-in:${editor.id}`,
            label: editor.label,
          })),
        ];
        const clicked = await api.contextMenu.show(
          buildPrimaryCardMenu({
            openIn: openInChildren,
            branchName,
            hasDefaultThread: primaryThreadKey !== null,
            pinned: primaryThreadKey !== null && pinnedThreadKeys.includes(primaryThreadKey),
            unread: primaryThreadKey !== null && unreadThreadKeys.includes(primaryThreadKey),
          }),
          position,
        );

        if (clicked === "pull") {
          const pullResult = await pullWorkspaceRow({
            environmentId: project.environmentId,
            input: { cwd },
          });
          if (pullResult._tag === "Failure") {
            if (!isAtomCommandInterrupted(pullResult)) {
              const error = squashAtomCommandFailure(pullResult);
              toastManager.add(
                stackedThreadToast({
                  type: "error",
                  title: "Failed to pull",
                  description: error instanceof Error ? error.message : "An error occurred.",
                }),
              );
            }
            return;
          }
          await refreshVcsStatusAfterPull({ environmentId: project.environmentId, input: { cwd } });
          return;
        }

        if (clicked === "open-in:file-explorer") {
          await openWorkspaceInFileManager(cwd);
          return;
        }

        if (typeof clicked === "string" && clicked.startsWith("open-in:")) {
          const editor = openInEditorOptions.find(
            (candidate) => `open-in:${candidate.id}` === clicked,
          );
          if (!editor) return;
          const openResult = await openInEditorMutation({
            environmentId: project.environmentId,
            input: { cwd, editor: editor.id },
          });
          if (openResult._tag === "Failure" && !isAtomCommandInterrupted(openResult)) {
            const error = squashAtomCommandFailure(openResult);
            toastManager.add(
              stackedThreadToast({
                type: "error",
                title: "Failed to open editor",
                description: error instanceof Error ? error.message : "An error occurred.",
              }),
            );
          }
          return;
        }

        if (clicked === "copy-path") {
          copyPathToClipboard(cwd, { path: cwd });
          return;
        }
        if (clicked === "copy-branch-name") {
          if (branchName !== null) {
            copyBranchNameToClipboard(branchName, { branch: branchName });
          }
          return;
        }
        if (clicked === "mark-unread" && primaryThreadKey) {
          markThreadRowUnread(primaryThreadKey);
          return;
        }
        if (clicked === "mark-read" && primaryThreadKey) {
          markThreadRowRead(primaryThreadKey);
          return;
        }
        if (clicked === "toggle-pin" && primaryThreadKey) {
          togglePinnedThreadKey(primaryThreadKey);
        }
      })();
    },
    [
      availableEditorsFor,
      copyBranchNameToClipboard,
      copyPathToClipboard,
      markThreadRowRead,
      markThreadRowUnread,
      openInEditorMutation,
      openWorkspaceInFileManager,
      pinnedThreadKeys,
      primaryEnvironmentId,
      primaryThread,
      project,
      pullWorkspaceRow,
      refreshVcsStatusAfterPull,
      togglePinnedThreadKey,
      unreadThreadKeys,
    ],
  );

  const showsSandboxBadge =
    !environmentCard &&
    project.environmentPresence === "remote-only" &&
    project.allRemoteMembersAreDesktopLocal;

  return (
    <>
      <div
        className={cn(
          "group/project-header relative",
          environmentCard && "rounded-md bg-sidebar-env-band",
          environmentCard && !environmentCard.available && "opacity-60",
        )}
      >
        <SidebarMenuButton
          ref={isManualProjectSorting ? dragHandleProps?.setActivatorNodeRef : undefined}
          aria-expanded={projectExpanded}
          aria-current={moduleRouteActive ? "page" : undefined}
          data-selected={projectSelected}
          isActive={moduleRouteActive || projectSelected}
          size="sm"
          className={projectHeaderButtonClassName({
            showsSandboxBadge,
            isManualProjectSorting,
            isEnvironmentCard: environmentCard !== null,
          })}
          {...(isManualProjectSorting && dragHandleProps ? dragHandleProps.attributes : {})}
          {...(isManualProjectSorting && dragHandleProps ? dragHandleProps.listeners : {})}
          onPointerDownCapture={handleProjectButtonPointerDownCapture}
          onClick={handleProjectButtonClick}
          onKeyDown={handleProjectButtonKeyDown}
          onContextMenu={handleProjectButtonContextMenu}
        >
          {!projectExpanded && projectStatus ? (
            <span className="-ml-0.5 relative inline-flex size-3.5 shrink-0 items-center justify-center">
              <span className="absolute inset-0 flex items-center justify-center transition-opacity duration-150 group-hover/project-header:opacity-0">
                <WorkspaceCardStatusGlyph status={projectStatus} />
              </span>
              <ChevronRightIcon className="absolute inset-0 m-auto size-3.5 text-muted-foreground/70 opacity-0 transition-opacity duration-150 group-hover/project-header:opacity-100" />
            </span>
          ) : (
            <ChevronRightIcon
              className={`-ml-0.5 size-3.5 shrink-0 text-muted-foreground/70 transition-transform duration-150 ${
                projectExpanded ? "rotate-90" : ""
              }`}
            />
          )}
          {environmentCard ? (
            <EnvironmentCardHeader
              identity={environmentCard}
              workspaceRoot={project.workspaceRoot}
            />
          ) : (
            <>
              <ProjectFavicon environmentId={project.environmentId} cwd={project.workspaceRoot} />
              <span className="flex min-w-0 flex-1 items-center gap-2">
                <span className="truncate text-[13px] font-medium text-foreground/90">
                  {project.displayName}
                </span>
                {project.groupedProjectCount > 1 ? (
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {project.groupedProjectCount} projects
                  </span>
                ) : null}
              </span>
            </>
          )}
        </SidebarMenuButton>
        {/* The container badge tells WSL projects apart inside Local, which
            mixes this device and WSL. A saved server's scope shows only that
            server's projects, so a cloud there said nothing and is gone. The
            badge crossfades with the hover strip. An environment card already
            names its environment, so it shows no badge. */}
        {showsSandboxBadge ? (
          <Tooltip>
            <TooltipTrigger
              render={
                <span
                  aria-label="Local sandbox project"
                  className="pointer-events-none absolute top-1 right-1.5 inline-flex size-5 items-center justify-center rounded-md text-muted-foreground/60 transition-opacity duration-150 max-sm:right-14 group-hover/project-header:opacity-0 group-focus-within/project-header:opacity-0 max-sm:group-hover/project-header:opacity-100 max-sm:group-focus-within/project-header:opacity-100"
                />
              }
            >
              <ContainerIcon className="size-3" />
            </TooltipTrigger>
            <TooltipPopup side="top">
              {`Local sandbox: ${project.remoteEnvironmentLabels.join(", ")}`}
            </TooltipPopup>
          </Tooltip>
        ) : null}
        <div className="pointer-events-none absolute top-[calc(50%+1px)] right-0.5 flex -translate-y-1/2 items-center gap-0.5 opacity-0 transition-opacity duration-150 max-sm:pointer-events-auto max-sm:opacity-100 group-hover/project-header:pointer-events-auto group-hover/project-header:opacity-100 group-focus-within/project-header:pointer-events-auto group-focus-within/project-header:opacity-100">
          <Tooltip>
            <TooltipTrigger
              render={
                <button
                  type="button"
                  aria-label={`Project actions for ${project.displayName}`}
                  aria-haspopup="menu"
                  data-testid="project-actions-button"
                  className={SIDEBAR_ICON_ACTION_BUTTON_CLASS}
                  onClick={handleProjectActionsClick}
                />
              }
            >
              <EllipsisIcon className="size-3.5" />
            </TooltipTrigger>
            <TooltipPopup side="top">Project actions</TooltipPopup>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger
              render={
                <button
                  type="button"
                  aria-label={`New worktree in ${project.displayName}`}
                  data-testid="new-worktree-button"
                  className={SIDEBAR_ICON_ACTION_BUTTON_CLASS}
                  onClick={handleCreateWorktreeClick}
                />
              }
            >
              <PlusIcon className="size-3.5" />
            </TooltipTrigger>
            <TooltipPopup side="top">New worktree</TooltipPopup>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger
              render={
                <button
                  type="button"
                  aria-label={`Git Manager for ${project.displayName}`}
                  data-testid="git-manager-button"
                  className={SIDEBAR_ICON_ACTION_BUTTON_CLASS}
                  onClick={handleOpenGitManagerClick}
                />
              }
            >
              <GitBranchIcon aria-hidden="true" className="size-3.5" />
            </TooltipTrigger>
            <TooltipPopup side="top">Git Manager</TooltipPopup>
          </Tooltip>
          {pullRequestsEnabled ? (
            <Tooltip>
              <TooltipTrigger
                render={
                  <button
                    type="button"
                    aria-label={`Pull Requests for ${project.displayName}`}
                    data-testid="pull-requests-button"
                    className={SIDEBAR_ICON_ACTION_BUTTON_CLASS}
                    onClick={handleOpenPullRequestsClick}
                  />
                }
              >
                <GitPullRequestIcon aria-hidden="true" className="size-3.5" />
              </TooltipTrigger>
              <TooltipPopup side="top">Pull Requests</TooltipPopup>
            </Tooltip>
          ) : null}
        </div>
      </div>

      <SidebarProjectThreadList
        projectExpanded={projectExpanded}
        hasOverflowingThreads={hasOverflowingThreads}
        hiddenThreadStatus={hiddenThreadStatus}
        orderedProjectThreadKeys={orderedProjectThreadKeys}
        renderedThreads={renderedThreads}
        shouldShowThreadPanel={shouldShowThreadPanel}
        isThreadListExpanded={isThreadListExpanded}
        activeRouteThreadKey={activeRouteThreadKey}
        threadJumpLabelByKey={threadJumpLabelByKey}
        appSettingsConfirmThreadArchive={appSettingsConfirmThreadArchive}
        renamingThreadKey={renamingThreadKey}
        renamingTitle={renamingTitle}
        setRenamingTitle={setRenamingTitle}
        startThreadRename={startThreadRename}
        renamingInputRef={renamingInputRef}
        renamingCommittedRef={renamingCommittedRef}
        confirmingArchiveThreadKey={confirmingArchiveThreadKey}
        setConfirmingArchiveThreadKey={setConfirmingArchiveThreadKey}
        confirmArchiveButtonRefs={confirmArchiveButtonRefs}
        attachThreadListAutoAnimateRef={attachThreadListAutoAnimateRef}
        handleThreadClick={handleThreadClick}
        navigateToThread={navigateToThread}
        handleMultiSelectContextMenu={handleMultiSelectContextMenu}
        handleThreadContextMenu={handleThreadContextMenu}
        clearSelection={clearSelection}
        commitRename={commitRename}
        cancelRename={cancelRename}
        attemptArchiveThread={attemptArchiveThread}
        openPrLink={openPrLink}
        expandThreadListForProject={expandThreadListForProject}
        collapseThreadListForProject={collapseThreadListForProject}
        project={project}
        serverConfigs={serverConfigs}
        primaryThread={primaryThread}
        modelLabels={modelLabels}
        chatSummaries={chatSummaries}
        onPrimaryClick={handlePrimaryRowClick}
        openPrimaryCardMenu={openPrimaryCardMenu}
        showDiscovery={supportedWorktreeDiscoveryMembers.length > 0}
        primaryEnvironmentId={primaryEnvironmentId}
        onDiscoveryHiddenCountChange={handleDiscoveryHiddenCountChange}
      />

      <ImportCliSessionsDialog
        open={importSessionsTarget !== null}
        target={importSessionsTarget}
        onOpenChange={handleImportSessionsOpenChange}
        onImported={handleSessionsImported}
        onOpenThread={openImportedThread}
      />

      <Dialog
        open={projectRenameTarget !== null}
        onOpenChange={(open) => {
          if (!open) {
            closeProjectRenameDialog();
          }
        }}
      >
        <DialogPopup className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Rename project</DialogTitle>
            <DialogDescription>
              {projectRenameTarget
                ? `Update the title for ${projectRenameTarget.workspaceRoot}.`
                : "Update the project title."}
            </DialogDescription>
          </DialogHeader>
          <DialogPanel className="space-y-4">
            <div className="grid gap-1.5">
              <span className="text-xs font-medium text-foreground">Project title</span>
              <Input
                aria-label="Project title"
                value={projectRenameTitle}
                onChange={(event) => setProjectRenameTitle(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void submitProjectRename();
                  }
                }}
              />
            </div>
            {projectRenameTarget?.environmentLabel ? (
              <p className="text-xs text-muted-foreground">
                Environment: {projectRenameTarget.environmentLabel}
              </p>
            ) : null}
          </DialogPanel>
          <DialogFooter>
            <Button variant="outline" onClick={closeProjectRenameDialog}>
              Cancel
            </Button>
            <Button onClick={() => void submitProjectRename()}>Save</Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>

      <Dialog
        open={projectGroupingTarget !== null}
        onOpenChange={(open) => {
          if (!open) {
            closeProjectGroupingDialog();
          }
        }}
      >
        <DialogPopup className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Project grouping</DialogTitle>
            <DialogDescription>
              {projectGroupingTarget
                ? `Choose how ${projectGroupingTarget.workspaceRoot} should be grouped in the sidebar.`
                : "Choose how this project should be grouped in the sidebar."}
            </DialogDescription>
          </DialogHeader>
          <DialogPanel className="space-y-4">
            <div className="grid gap-1.5">
              <span className="text-xs font-medium text-foreground">Grouping rule</span>
              <Select
                value={projectGroupingSelection}
                onValueChange={(value) => {
                  if (
                    value === "inherit" ||
                    value === "repository" ||
                    value === "repository_path" ||
                    value === "separate"
                  ) {
                    setProjectGroupingSelection(value);
                  }
                }}
              >
                <SelectTrigger className="w-full" aria-label="Project grouping rule">
                  <SelectValue>
                    {projectGroupingSelection === "inherit"
                      ? `Use global default (${PROJECT_GROUPING_MODE_LABELS[projectGroupingSettings.sidebarProjectGroupingMode]})`
                      : PROJECT_GROUPING_MODE_LABELS[projectGroupingSelection]}
                  </SelectValue>
                </SelectTrigger>
                <SelectPopup align="end" alignItemWithTrigger={false}>
                  <SelectItem hideIndicator value="inherit">
                    Use global default
                  </SelectItem>
                  <SelectItem hideIndicator value="repository">
                    {PROJECT_GROUPING_MODE_LABELS.repository}
                  </SelectItem>
                  <SelectItem hideIndicator value="repository_path">
                    {PROJECT_GROUPING_MODE_LABELS.repository_path}
                  </SelectItem>
                  <SelectItem hideIndicator value="separate">
                    {PROJECT_GROUPING_MODE_LABELS.separate}
                  </SelectItem>
                </SelectPopup>
              </Select>
            </div>
            <p className="text-xs text-muted-foreground">
              {projectGroupingSelection === "inherit"
                ? projectGroupingModeDescription(projectGroupingSettings.sidebarProjectGroupingMode)
                : projectGroupingModeDescription(projectGroupingSelection)}
            </p>
          </DialogPanel>
          <DialogFooter>
            <Button variant="outline" onClick={closeProjectGroupingDialog}>
              Cancel
            </Button>
            <Button onClick={saveProjectGroupingPreference}>Save</Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    </>
  );
});

const SidebarProjectListRow = memo(function SidebarProjectListRow(props: SidebarProjectItemProps) {
  // Inside a repository card an environment checkout is a band, not a nested card.
  const isCard = !props.environmentCard;
  return (
    <SidebarMenuItem
      className={isCard ? cn(SIDEBAR_CARD_CLASS, "p-1") : "rounded-md"}
      data-sidebar-card={isCard ? "true" : undefined}
    >
      <SidebarProjectItem {...props} />
    </SidebarMenuItem>
  );
});

function LocalSecondaryStatus() {
  const { environments } = useEnvironments();
  // The desktop reports which local secondary backends (e.g. the WSL backend)
  // exist; the hook polls because the bridge has no change event. A backend that
  // is still cold-booting has no httpBaseUrl yet and isn't in the catalog, so we
  // surface "Connecting" straight from the bootstrap list and clear it once the
  // matching environment reports a connected phase.
  const secondaries = useDesktopLocalBootstraps();

  // Connected desktop-local environments keyed by their backend URL so we can
  // match a bootstrap (which only knows the URL) to its connection phase.
  const localEnvByUrl = useMemo(() => {
    const map = new Map<string, { phase: string; error: string | null }>();
    for (const environment of environments) {
      if (
        isDesktopLocalConnectionTarget(environment.entry.target) &&
        environment.displayUrl !== null
      ) {
        map.set(environment.displayUrl, {
          phase: environment.connection.phase,
          error: environment.connection.error,
        });
      }
    }
    return map;
  }, [environments]);

  const connecting: string[] = [];
  const failed: Array<{ label: string; error: string | null }> = [];
  for (const bootstrap of secondaries) {
    if (bootstrap.preflightError) {
      failed.push({ label: bootstrap.label, error: bootstrap.preflightError.detail });
      continue;
    }
    const env =
      bootstrap.httpBaseUrl !== null ? localEnvByUrl.get(bootstrap.httpBaseUrl) : undefined;
    if (env?.phase === "connected") {
      continue;
    }
    if (env?.phase === "error") {
      failed.push({ label: bootstrap.label, error: env.error });
      continue;
    }
    connecting.push(bootstrap.label);
  }

  if (connecting.length === 0 && failed.length === 0) {
    return null;
  }

  return (
    <SidebarGroup className="px-2 pt-2 pb-0">
      {connecting.length > 0 ? (
        <Alert
          variant="default"
          className="rounded-2xl border-border/40 bg-accent/40 text-muted-foreground"
        >
          <LoaderIcon className="animate-spin" />
          <AlertTitle className="text-xs font-medium text-foreground">
            Connecting {connecting.join(", ")}
          </AlertTitle>
        </Alert>
      ) : null}
      {failed.length > 0 ? (
        <Alert variant="warning" className="rounded-2xl border-warning/40 bg-warning/8">
          <TriangleAlertIcon />
          <AlertTitle>Couldn't connect {failed.map((entry) => entry.label).join(", ")}</AlertTitle>
          <AlertDescription className="min-w-0 wrap-anywhere">
            {failed
              .map((entry) => entry.error)
              .filter(Boolean)
              .join("; ") || "The backend didn't respond."}
          </AlertDescription>
        </Alert>
      ) : null}
    </SidebarGroup>
  );
}

type SortableProjectHandleProps = Pick<
  ReturnType<typeof useSortable>,
  "attributes" | "listeners" | "setActivatorNodeRef"
>;

function ProjectSortMenu({
  projectSortOrder,
  repositorySortOrder,
  threadSortOrder,
  projectGroupingMode,
  threadPreviewCount,
  onProjectSortOrderChange,
  onRepositorySortOrderChange,
  onThreadSortOrderChange,
  onProjectGroupingModeChange,
  onThreadPreviewCountChange,
}: {
  projectSortOrder: SidebarProjectSortOrder;
  // Non-null in the Repositories view, which sorts repositories instead of projects.
  repositorySortOrder: SidebarRepositorySortOrder | null;
  threadSortOrder: SidebarThreadSortOrder;
  // Null hides the grouping choice (the Repositories view always groups by repository).
  projectGroupingMode: SidebarProjectGroupingMode | null;
  threadPreviewCount: SidebarThreadPreviewCount;
  onProjectSortOrderChange: (sortOrder: SidebarProjectSortOrder) => void;
  onRepositorySortOrderChange: (sortOrder: SidebarRepositorySortOrder) => void;
  onThreadSortOrderChange: (sortOrder: SidebarThreadSortOrder) => void;
  onProjectGroupingModeChange: (mode: SidebarProjectGroupingMode) => void;
  onThreadPreviewCountChange: (count: SidebarThreadPreviewCount) => void;
}) {
  const handleThreadPreviewCountChange = useCallback(
    (nextValue: number | null) => {
      if (nextValue === null) {
        return;
      }

      const clampedValue = clampSidebarThreadPreviewCount(nextValue);
      if (clampedValue !== threadPreviewCount) {
        onThreadPreviewCountChange(clampedValue);
      }
    },
    [onThreadPreviewCountChange, threadPreviewCount],
  );

  return (
    <Menu>
      <Tooltip>
        <TooltipTrigger
          render={
            <MenuTrigger className="inline-flex h-6 min-w-6 cursor-pointer items-center justify-center rounded-md px-[calc(--spacing(1)-1px)] text-muted-foreground/60 transition-colors hover:bg-accent hover:text-foreground" />
          }
        >
          <ArrowUpDownIcon className="size-3.5" />
        </TooltipTrigger>
        <TooltipPopup side="right">Sidebar options</TooltipPopup>
      </Tooltip>
      <MenuPopup align="end" side="bottom" className="min-w-52">
        {repositorySortOrder === null ? (
          <MenuGroup>
            <div className="px-2 py-1 sm:text-xs font-medium text-muted-foreground">
              Sort projects
            </div>
            <MenuRadioGroup
              value={projectSortOrder}
              onValueChange={(value) => {
                onProjectSortOrderChange(value as SidebarProjectSortOrder);
              }}
            >
              {(
                Object.entries(SIDEBAR_SORT_LABELS) as Array<[SidebarProjectSortOrder, string]>
              ).map(([value, label]) => (
                <MenuRadioItem key={value} value={value} className="min-h-7 py-1 sm:text-xs">
                  {label}
                </MenuRadioItem>
              ))}
            </MenuRadioGroup>
          </MenuGroup>
        ) : (
          <MenuGroup>
            <div className="px-2 py-1 sm:text-xs font-medium text-muted-foreground">
              Sort repositories
            </div>
            <MenuRadioGroup
              value={repositorySortOrder}
              onValueChange={(value) => {
                onRepositorySortOrderChange(value as SidebarRepositorySortOrder);
              }}
            >
              {(
                Object.entries(SIDEBAR_REPOSITORY_SORT_LABELS) as Array<
                  [SidebarRepositorySortOrder, string]
                >
              ).map(([value, label]) => (
                <MenuRadioItem key={value} value={value} className="min-h-7 py-1 sm:text-xs">
                  {label}
                </MenuRadioItem>
              ))}
            </MenuRadioGroup>
          </MenuGroup>
        )}
        <MenuGroup>
          <div className="px-2 pt-2 pb-1 sm:text-xs font-medium text-muted-foreground">
            Sort threads
          </div>
          <MenuRadioGroup
            value={threadSortOrder}
            onValueChange={(value) => {
              onThreadSortOrderChange(value as SidebarThreadSortOrder);
            }}
          >
            {(
              Object.entries(SIDEBAR_THREAD_SORT_LABELS) as Array<[SidebarThreadSortOrder, string]>
            ).map(([value, label]) => (
              <MenuRadioItem key={value} value={value} className="min-h-7 py-1 sm:text-xs">
                {label}
              </MenuRadioItem>
            ))}
          </MenuRadioGroup>
        </MenuGroup>
        <MenuGroup>
          <div className="px-2 pt-2 pb-1 text-muted-foreground sm:text-xs font-medium">
            Visible threads
          </div>
          <div className="px-2 py-1">
            <NumberField
              aria-label="Visible thread count"
              className="w-28 gap-0"
              max={MAX_SIDEBAR_THREAD_PREVIEW_COUNT}
              min={MIN_SIDEBAR_THREAD_PREVIEW_COUNT}
              onValueChange={handleThreadPreviewCountChange}
              size="sm"
              step={1}
              value={threadPreviewCount}
            >
              <NumberFieldGroup className="h-7 rounded-md sm:h-6.5">
                <NumberFieldDecrement
                  aria-label="Decrease visible thread count"
                  className="px-2 sm:px-2 [&_svg]:size-3.5"
                />
                <NumberFieldInput
                  aria-label="Visible thread count"
                  className="h-7 w-9 grow-0 px-0 text-xs leading-7 sm:h-6.5 sm:leading-6.5"
                  inputMode="numeric"
                  onKeyDownCapture={(event) => {
                    event.stopPropagation();
                  }}
                />
                <NumberFieldIncrement
                  aria-label="Increase visible thread count"
                  className="px-2 sm:px-2 [&_svg]:size-3.5"
                />
              </NumberFieldGroup>
            </NumberField>
          </div>
        </MenuGroup>
        {projectGroupingMode === null ? null : (
          <>
            <MenuSeparator />
            <MenuGroup>
              <div className="px-2 pt-2 pb-1 font-medium text-muted-foreground sm:text-xs">
                Group projects
              </div>
              <MenuRadioGroup
                value={projectGroupingMode}
                onValueChange={(value) => {
                  if (
                    value === "repository" ||
                    value === "repository_path" ||
                    value === "separate"
                  ) {
                    onProjectGroupingModeChange(value);
                  }
                }}
              >
                {(
                  Object.entries(PROJECT_GROUPING_MODE_LABELS) as Array<
                    [SidebarProjectGroupingMode, string]
                  >
                ).map(([value, label]) => (
                  <MenuRadioItem key={value} value={value} className="min-h-7 py-1 sm:text-xs">
                    {label}
                  </MenuRadioItem>
                ))}
              </MenuRadioGroup>
            </MenuGroup>
          </>
        )}
      </MenuPopup>
    </Menu>
  );
}

function SortableProjectItem({
  projectId,
  disabled = false,
  children,
}: {
  projectId: string;
  disabled?: boolean;
  children: (handleProps: SortableProjectHandleProps) => React.ReactNode;
}) {
  const {
    attributes,
    listeners,
    setActivatorNodeRef,
    setNodeRef,
    transform,
    transition,
    isDragging,
    isOver,
  } = useSortable({ id: projectId, disabled });
  return (
    <li
      ref={setNodeRef}
      style={{
        transform: CSS.Translate.toString(transform),
        transition,
      }}
      className={cn(
        "group/menu-item relative p-1",
        SIDEBAR_CARD_CLASS,
        isDragging && "z-20 opacity-80",
        isOver && !isDragging && "ring-1 ring-primary/40",
      )}
      data-sidebar-card="true"
      data-sidebar="menu-item"
      data-slot="sidebar-menu-item"
    >
      {children({ attributes, listeners, setActivatorNodeRef })}
    </li>
  );
}

const SidebarChromeHeader = memo(function SidebarChromeHeader() {
  // The fixed sidebar toggle overlays the rail's top strip. With the rail
  // hidden (Repositories view) reserve that strip here so the brand stays put.
  // Below `md` the header carries its own trigger, so nothing is reserved.
  const railHidden = useUiStateStore((state) => state.sidebarView === "repositories");
  return (
    <SidebarHeader
      className={cn(
        "@container/sidebar-header h-[var(--workspace-topbar-height)] shrink-0 flex-row items-center border-b border-panel-separator px-3 py-0 md:px-0",
        railHidden && "md:pl-[var(--environment-rail-width)]",
      )}
      style={{ "--environment-rail-width": `${ENVIRONMENT_RAIL_WIDTH_PX}px` } as CSSProperties}
    >
      <SidebarTrigger className="md:hidden" />
      <SidebarBrand />
    </SidebarHeader>
  );
});

function SidebarBrand() {
  const stageLabel = useSidebarStageLabel();

  return (
    <Link
      aria-label="Go to threads"
      className="sidebar-brand ml-3 h-7 w-fit min-w-0 shrink-0 items-center gap-1 overflow-hidden rounded-md text-foreground outline-hidden ring-ring focus-visible:ring-2"
      to="/"
    >
      <SidebarBrandContent appBaseName={APP_BASE_NAME} stageLabel={stageLabel} />
    </Link>
  );
}

function useSidebarStageLabel() {
  const primaryServerVersion =
    useAtomValue(primaryServerConfigAtom)?.environment.serverVersion ?? null;

  return resolveSidebarStageBadgeLabel({
    primaryServerVersion,
    fallbackStageLabel: APP_STAGE_LABEL,
  });
}

export function SidebarBrandContent({
  appBaseName,
  stageLabel,
}: {
  readonly appBaseName: string;
  readonly stageLabel: string | null;
}) {
  return (
    <>
      <span className="truncate text-sm font-semibold text-foreground">{appBaseName}</span>
      {stageLabel ? (
        <span className="sidebar-brand-stage shrink-0 items-center whitespace-nowrap rounded-full bg-muted/50 px-1.5 py-0.5 text-xs font-medium uppercase text-muted-foreground">
          {stageLabel}
        </span>
      ) : null}
    </>
  );
}

const SidebarChromeFooter = memo(function SidebarChromeFooter() {
  const navigate = useNavigate();
  const { isMobile, setOpenMobile } = useSidebar();
  const handleSettingsClick = useCallback(() => {
    if (isMobile) {
      setOpenMobile(false);
    }
    void navigate({ to: "/settings" });
  }, [isMobile, navigate, setOpenMobile]);

  return (
    <SidebarFooter className="p-2">
      <SidebarProviderUpdatePill />
      <SidebarUpdatePill />
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton
            size="sm"
            className="gap-2 px-2 py-1.5 text-foreground/80 hover:bg-accent hover:text-foreground"
            onClick={handleSettingsClick}
          >
            <SettingsIcon className="size-4 text-foreground/60" aria-hidden />
            <span className="text-[13px] font-medium">Settings</span>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarFooter>
  );
});

interface SidebarProjectsContentProps {
  showArm64IntelBuildWarning: boolean;
  arm64IntelBuildWarningDescription: string | null;
  desktopUpdateButtonAction: "download" | "install" | "none";
  desktopUpdateButtonDisabled: boolean;
  handleDesktopUpdateButtonClick: () => void;
  projectSortOrder: SidebarProjectSortOrder;
  repositorySortOrder: SidebarRepositorySortOrder;
  threadSortOrder: SidebarThreadSortOrder;
  projectGroupingMode: SidebarProjectGroupingMode;
  threadPreviewCount: SidebarThreadPreviewCount;
  updateSettings: ReturnType<typeof useUpdateClientSettings>;
  openAddProject: () => void;
  addProjectLabel: string;
  isManualProjectSorting: boolean;
  projectDnDSensors: ReturnType<typeof useSensors>;
  projectCollisionDetection: CollisionDetection;
  handleProjectDragStart: (event: DragStartEvent) => void;
  handleProjectDragEnd: (event: DragEndEvent) => void;
  handleProjectDragCancel: (event: DragCancelEvent) => void;
  openCreateWorktreeDialog: (projectRef?: ScopedProjectRef | null) => void;
  archiveThread: ReturnType<typeof useThreadActions>["archiveThread"];
  deleteThread: ReturnType<typeof useThreadActions>["deleteThread"];
  sortedProjects: readonly SidebarProjectSnapshot[];
  repositoriesView: boolean;
  repositoryGroups: readonly RepositoryGroup[] | null;
  repositoryGroupExpandedById: Readonly<Record<string, boolean>>;
  environmentCardIdentities: ReadonlyMap<EnvironmentId, EnvironmentCardIdentity>;
  expandedThreadListsByProject: ReadonlySet<string>;
  activeRouteProjectKey: string | null;
  routeThreadKey: string | null;
  moduleRouteProjectKey: string | null;
  selectedProjectKey: string | null;
  selectProject: (projectKey: string) => void;
  commandPaletteShortcutLabel: string | null;
  threadJumpLabelByKey: ReadonlyMap<string, string>;
  attachThreadListAutoAnimateRef: (node: HTMLElement | null) => void;
  expandThreadListForProject: (projectKey: string) => void;
  collapseThreadListForProject: (projectKey: string) => void;
  dragInProgressRef: React.RefObject<boolean>;
  suppressProjectClickAfterDragRef: React.RefObject<boolean>;
  suppressProjectClickForContextMenuRef: React.RefObject<boolean>;
  attachProjectListAutoAnimateRef: (node: HTMLElement | null) => void;
  projectAvailability: ReturnType<typeof resolveSidebarProjectAvailability>;
  projectAvailabilityEnvironment: EnvironmentPresentation | null;
  showProjectAvailabilityRetry: boolean;
  showProjectAvailabilityConnectionSettings: boolean;
  showProjectAvailabilityOpenRemoteServers: boolean;
  onRetryProjectEnvironment: (environmentId: EnvironmentId) => void;
  onOpenProjectSettings: () => void;
  onViewProjectDiagnostics: () => void;
  onAdoptProjectStorage: (environmentId: EnvironmentId) => void;
  onRecoverProjectData?: ((environmentId: EnvironmentId) => void) | undefined;
}

const SidebarProjectsContent = memo(function SidebarProjectsContent(
  props: SidebarProjectsContentProps,
) {
  const {
    showArm64IntelBuildWarning,
    arm64IntelBuildWarningDescription,
    desktopUpdateButtonAction,
    desktopUpdateButtonDisabled,
    handleDesktopUpdateButtonClick,
    projectSortOrder,
    repositorySortOrder,
    threadSortOrder,
    projectGroupingMode,
    threadPreviewCount,
    updateSettings,
    openAddProject,
    addProjectLabel,
    isManualProjectSorting,
    projectDnDSensors,
    projectCollisionDetection,
    handleProjectDragStart,
    handleProjectDragEnd,
    handleProjectDragCancel,
    openCreateWorktreeDialog,
    archiveThread,
    deleteThread,
    sortedProjects,
    repositoriesView,
    repositoryGroups,
    repositoryGroupExpandedById,
    environmentCardIdentities,
    expandedThreadListsByProject,
    activeRouteProjectKey,
    routeThreadKey,
    moduleRouteProjectKey,
    selectedProjectKey,
    selectProject,
    commandPaletteShortcutLabel,
    threadJumpLabelByKey,
    attachThreadListAutoAnimateRef,
    expandThreadListForProject,
    collapseThreadListForProject,
    dragInProgressRef,
    suppressProjectClickAfterDragRef,
    suppressProjectClickForContextMenuRef,
    attachProjectListAutoAnimateRef,
    projectAvailability,
    projectAvailabilityEnvironment,
    showProjectAvailabilityRetry,
    showProjectAvailabilityConnectionSettings,
    showProjectAvailabilityOpenRemoteServers,
    onRetryProjectEnvironment,
    onOpenProjectSettings,
    onViewProjectDiagnostics,
    onAdoptProjectStorage,
    onRecoverProjectData,
  } = props;

  const handleProjectSortOrderChange = useCallback(
    (sortOrder: SidebarProjectSortOrder) => {
      updateSettings({ sidebarProjectSortOrder: sortOrder });
    },
    [updateSettings],
  );
  const handleRepositorySortOrderChange = useCallback(
    (sortOrder: SidebarRepositorySortOrder) => {
      updateSettings({ sidebarRepositorySortOrder: sortOrder });
    },
    [updateSettings],
  );
  const handleThreadSortOrderChange = useCallback(
    (sortOrder: SidebarThreadSortOrder) => {
      updateSettings({ sidebarThreadSortOrder: sortOrder });
    },
    [updateSettings],
  );
  const handleProjectGroupingModeChange = useCallback(
    (groupingMode: SidebarProjectGroupingMode) => {
      updateSettings({ sidebarProjectGroupingMode: groupingMode });
    },
    [updateSettings],
  );
  const handleThreadPreviewCountChange = useCallback(
    (count: SidebarThreadPreviewCount) => {
      updateSettings({ sidebarThreadPreviewCount: count });
    },
    [updateSettings],
  );

  return (
    <SidebarContent className="min-h-full gap-0">
      <SidebarGroup className="px-2 pt-2 pb-1">
        <SidebarMenu>
          <SidebarMenuItem>
            <CommandDialogTrigger
              render={
                <SidebarMenuButton
                  size="sm"
                  className="gap-2 px-2 py-1.5 text-foreground/80 hover:bg-accent hover:text-foreground focus-visible:ring-0"
                  data-testid="command-palette-trigger"
                />
              }
            >
              <SearchIcon className="size-4 text-foreground/60" />
              <span className="flex-1 truncate text-left text-[13px] font-medium">Search</span>
              {commandPaletteShortcutLabel ? (
                <Kbd className="h-4 min-w-0 rounded-sm px-1.5 text-xs">
                  {commandPaletteShortcutLabel}
                </Kbd>
              ) : null}
            </CommandDialogTrigger>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
      <AgentsNavRow />
      {showArm64IntelBuildWarning && arm64IntelBuildWarningDescription ? (
        <SidebarGroup className="px-2 pt-2 pb-0">
          <Alert variant="warning" className="rounded-2xl border-warning/40 bg-warning/8">
            <TriangleAlertIcon />
            <AlertTitle>Intel build on Apple Silicon</AlertTitle>
            <AlertDescription>{arm64IntelBuildWarningDescription}</AlertDescription>
            {desktopUpdateButtonAction !== "none" ? (
              <AlertAction>
                <Button
                  size="xs"
                  variant="outline"
                  disabled={desktopUpdateButtonDisabled}
                  onClick={handleDesktopUpdateButtonClick}
                >
                  {desktopUpdateButtonAction === "download"
                    ? "Download ARM build"
                    : "Install ARM build"}
                </Button>
              </AlertAction>
            ) : null}
          </Alert>
        </SidebarGroup>
      ) : null}
      <LocalSecondaryStatus />
      <SidebarGroup
        className="flex-1 bg-sidebar-well px-2 py-2"
        data-testid="sidebar-projects-group"
      >
        <div className="mb-1 flex items-center justify-between pl-2 pr-1.5">
          <span className="text-xs font-medium text-muted-foreground">
            {repositoriesView ? "Repositories · all environments" : "Projects"}
          </span>
          <div className="flex items-center gap-1">
            <ProjectSortMenu
              projectSortOrder={projectSortOrder}
              repositorySortOrder={repositoriesView ? repositorySortOrder : null}
              threadSortOrder={threadSortOrder}
              projectGroupingMode={repositoriesView ? null : projectGroupingMode}
              threadPreviewCount={threadPreviewCount}
              onProjectSortOrderChange={handleProjectSortOrderChange}
              onRepositorySortOrderChange={handleRepositorySortOrderChange}
              onThreadSortOrderChange={handleThreadSortOrderChange}
              onProjectGroupingModeChange={handleProjectGroupingModeChange}
              onThreadPreviewCountChange={handleThreadPreviewCountChange}
            />
            <Tooltip>
              <TooltipTrigger
                render={
                  <button
                    type="button"
                    aria-label={addProjectLabel}
                    data-testid="sidebar-add-project-trigger"
                    className="inline-flex h-6 min-w-6 cursor-pointer items-center justify-center rounded-md px-[calc(--spacing(1)-1px)] text-muted-foreground/60 transition-colors hover:bg-accent hover:text-foreground"
                    onClick={openAddProject}
                  />
                }
              >
                <FolderPlusIcon className="size-3.5" />
              </TooltipTrigger>
              <TooltipPopup side="right">{addProjectLabel}</TooltipPopup>
            </Tooltip>
          </div>
        </div>

        {repositoryGroups !== null ? (
          <SidebarMenu className="gap-2" data-testid="sidebar-project-list">
            {repositoryGroups.map((group) => (
              <SidebarRepositoryGroup
                key={group.key}
                group={group}
                expanded={repositoryGroupExpandedById[group.key] !== false}
              >
                {group.cards.map((project) => (
                  <SidebarProjectListRow
                    key={project.projectKey}
                    project={project}
                    environmentCard={environmentCardIdentities.get(project.environmentId) ?? null}
                    isThreadListExpanded={expandedThreadListsByProject.has(project.projectKey)}
                    activeRouteThreadKey={
                      activeRouteProjectKey === project.projectKey ? routeThreadKey : null
                    }
                    moduleRouteActive={moduleRouteProjectKey === project.projectKey}
                    selectedProjectKey={selectedProjectKey}
                    selectProject={selectProject}
                    openCreateWorktreeDialog={openCreateWorktreeDialog}
                    archiveThread={archiveThread}
                    deleteThread={deleteThread}
                    threadJumpLabelByKey={threadJumpLabelByKey}
                    attachThreadListAutoAnimateRef={attachThreadListAutoAnimateRef}
                    expandThreadListForProject={expandThreadListForProject}
                    collapseThreadListForProject={collapseThreadListForProject}
                    dragInProgressRef={dragInProgressRef}
                    suppressProjectClickAfterDragRef={suppressProjectClickAfterDragRef}
                    suppressProjectClickForContextMenuRef={suppressProjectClickForContextMenuRef}
                    isManualProjectSorting={false}
                    dragHandleProps={null}
                  />
                ))}
              </SidebarRepositoryGroup>
            ))}
          </SidebarMenu>
        ) : isManualProjectSorting ? (
          <DndContext
            sensors={projectDnDSensors}
            collisionDetection={projectCollisionDetection}
            modifiers={[restrictToVerticalAxis, restrictToFirstScrollableAncestor]}
            onDragStart={handleProjectDragStart}
            onDragEnd={handleProjectDragEnd}
            onDragCancel={handleProjectDragCancel}
          >
            <SidebarMenu className="gap-1.5" data-testid="sidebar-project-list">
              <SortableContext
                items={sortedProjects.map((project) => project.projectKey)}
                strategy={verticalListSortingStrategy}
              >
                {sortedProjects.map((project) => (
                  <SortableProjectItem key={project.projectKey} projectId={project.projectKey}>
                    {(dragHandleProps) => (
                      <SidebarProjectItem
                        project={project}
                        isThreadListExpanded={expandedThreadListsByProject.has(project.projectKey)}
                        activeRouteThreadKey={
                          activeRouteProjectKey === project.projectKey ? routeThreadKey : null
                        }
                        moduleRouteActive={moduleRouteProjectKey === project.projectKey}
                        selectedProjectKey={selectedProjectKey}
                        selectProject={selectProject}
                        openCreateWorktreeDialog={openCreateWorktreeDialog}
                        archiveThread={archiveThread}
                        deleteThread={deleteThread}
                        threadJumpLabelByKey={threadJumpLabelByKey}
                        attachThreadListAutoAnimateRef={attachThreadListAutoAnimateRef}
                        expandThreadListForProject={expandThreadListForProject}
                        collapseThreadListForProject={collapseThreadListForProject}
                        dragInProgressRef={dragInProgressRef}
                        suppressProjectClickAfterDragRef={suppressProjectClickAfterDragRef}
                        suppressProjectClickForContextMenuRef={
                          suppressProjectClickForContextMenuRef
                        }
                        isManualProjectSorting={isManualProjectSorting}
                        dragHandleProps={dragHandleProps}
                      />
                    )}
                  </SortableProjectItem>
                ))}
              </SortableContext>
            </SidebarMenu>
          </DndContext>
        ) : (
          <SidebarMenu
            ref={attachProjectListAutoAnimateRef}
            className="gap-1.5"
            data-testid="sidebar-project-list"
          >
            {sortedProjects.map((project) => (
              <SidebarProjectListRow
                key={project.projectKey}
                project={project}
                isThreadListExpanded={expandedThreadListsByProject.has(project.projectKey)}
                activeRouteThreadKey={
                  activeRouteProjectKey === project.projectKey ? routeThreadKey : null
                }
                moduleRouteActive={moduleRouteProjectKey === project.projectKey}
                selectedProjectKey={selectedProjectKey}
                selectProject={selectProject}
                openCreateWorktreeDialog={openCreateWorktreeDialog}
                archiveThread={archiveThread}
                deleteThread={deleteThread}
                threadJumpLabelByKey={threadJumpLabelByKey}
                attachThreadListAutoAnimateRef={attachThreadListAutoAnimateRef}
                expandThreadListForProject={expandThreadListForProject}
                collapseThreadListForProject={collapseThreadListForProject}
                dragInProgressRef={dragInProgressRef}
                suppressProjectClickAfterDragRef={suppressProjectClickAfterDragRef}
                suppressProjectClickForContextMenuRef={suppressProjectClickForContextMenuRef}
                isManualProjectSorting={isManualProjectSorting}
                dragHandleProps={null}
              />
            ))}
          </SidebarMenu>
        )}

        {repositoriesView && projectAvailability.kind === "empty-confirmed" ? (
          <RepositoriesViewEmptyState onAddProject={openAddProject} />
        ) : (
          <SidebarProjectAvailability
            view={projectAvailability}
            environment={projectAvailabilityEnvironment}
            showRetry={showProjectAvailabilityRetry}
            showConnectionSettings={showProjectAvailabilityConnectionSettings}
            showOpenRemoteServers={showProjectAvailabilityOpenRemoteServers}
            onRetry={onRetryProjectEnvironment}
            onOpenSettings={onOpenProjectSettings}
            onViewDiagnostics={onViewProjectDiagnostics}
            onAdoptStorage={onAdoptProjectStorage}
            onRecoverData={onRecoverProjectData}
          />
        )}
      </SidebarGroup>
    </SidebarContent>
  );
});

/** The add-project dialog asks for the environment, so this view can start it directly. */
function RepositoriesViewEmptyState({ onAddProject }: { onAddProject: () => void }) {
  return (
    <div className="px-2 pt-4 text-center text-xs text-muted-foreground">
      <div>No projects yet.</div>
      <div className="mt-2 flex justify-center">
        <Button size="xs" variant="outline" onClick={onAddProject}>
          Add project
        </Button>
      </div>
    </div>
  );
}

const NO_ENVIRONMENT_CARD_IDENTITIES: ReadonlyMap<EnvironmentId, EnvironmentCardIdentity> =
  new Map();
const NO_REPOSITORY_GROUP_EXPANSION: Readonly<Record<string, boolean>> = {};

const SEPARATE_PROJECT_GROUPING: ProjectGroupingSettings = {
  sidebarProjectGroupingMode: "separate",
  sidebarProjectGroupingOverrides: {},
};

export default function Sidebar() {
  const allProjects = useProjects();
  const { environments } = useEnvironments();
  const activeEnvironmentId = useActiveEnvironmentId();
  const visibleEnvironmentIds = useMemo(
    () =>
      selectRailVisibleEnvironmentIds({
        activeEnvironmentId,
        candidates: environments.map((environment) => ({
          environmentId: environment.environmentId,
          isLocal:
            environment.entry.target._tag === "PrimaryConnectionTarget" ||
            isDesktopLocalConnectionTarget(environment.entry.target),
        })),
      }),
    [activeEnvironmentId, environments],
  );
  const sidebarView = useUiStateStore((store) => store.sidebarView);
  const repositoriesView = sidebarView === "repositories";
  // The Repositories view hides the rail, so naming its selection would point at nothing.
  const addProjectLabel = useMemo(() => {
    const remoteLabel = resolveAddProjectTargetLabel({
      activeEnvironmentId,
      candidates: environments.map((environment) => ({
        environmentId: environment.environmentId,
        label: environment.label,
        isLocal:
          environment.entry.target._tag === "PrimaryConnectionTarget" ||
          isDesktopLocalConnectionTarget(environment.entry.target),
      })),
    });
    return remoteLabel === null || repositoriesView
      ? "Add project"
      : `Add project on ${remoteLabel}`;
  }, [activeEnvironmentId, environments, repositoriesView]);
  // The Repositories view lists every environment; the rail filter applies only to Environments.
  const projects = useMemo(
    () =>
      repositoriesView || visibleEnvironmentIds === null
        ? allProjects
        : allProjects.filter((project) => visibleEnvironmentIds.has(project.environmentId)),
    [allProjects, repositoriesView, visibleEnvironmentIds],
  );
  const presentation = useMemo(() => readCurrentEnvironmentPresentationPolicy(), []);
  const shellSummary = useEnvironmentShellSummary();
  const allSidebarThreads = useThreadShells();
  const sidebarThreads = useMemo(
    () =>
      repositoriesView || visibleEnvironmentIds === null
        ? allSidebarThreads
        : allSidebarThreads.filter((thread) => visibleEnvironmentIds.has(thread.environmentId)),
    [allSidebarThreads, repositoriesView, visibleEnvironmentIds],
  );
  const projectExpandedById = useUiStateStore((store) => store.projectExpandedById);
  const projectOrder = useUiStateStore((store) => store.projectOrder);
  const reorderProjects = useUiStateStore((store) => store.reorderProjects);
  const navigate = useNavigate();
  const retryProjectEnvironment = useAtomCommand(environmentAvailabilityCommands.retry, {
    reportFailure: false,
  });
  const adoptProjectStorage = useAtomCommand(environmentAvailabilityCommands.adoptStorage, {
    reportFailure: false,
  });
  const pathname = useLocation({ select: (loc) => loc.pathname });
  const isOnSettings = pathname.startsWith("/settings");
  const sidebarThreadSortOrder = useClientSettings((s) => s.sidebarThreadSortOrder);
  const sidebarProjectSortOrder = useClientSettings((s) => s.sidebarProjectSortOrder);
  const sidebarRepositorySortOrder = useClientSettings((s) => s.sidebarRepositorySortOrder);
  // Repository groups take the order of their projects, so timestamp orders sort projects by it.
  const effectiveProjectSortOrder: SidebarProjectSortOrder = repositoriesView
    ? sidebarRepositorySortOrder === "name"
      ? "updated_at"
      : sidebarRepositorySortOrder
    : sidebarProjectSortOrder;
  const sidebarProjectGroupingMode = useClientSettings((s) => s.sidebarProjectGroupingMode);
  const projectGroupingSettings = useClientSettings(selectProjectGroupingSettings);
  // One project node per environment checkout; repositories are grouped at render time.
  const effectiveGroupingSettings = repositoriesView
    ? SEPARATE_PROJECT_GROUPING
    : projectGroupingSettings;
  const sidebarThreadPreviewCount = useClientSettings((s) => s.sidebarThreadPreviewCount);
  const updateSettings = useUpdateClientSettings();
  // "New workspace" entry points open CreateWorktreeDialog
  // instead of seeding a draft thread. `createWorktreeDialogProjectRef` is the
  // project the dialog's "Project" select defaults to (null = the global "+"
  // entry point, no preselection) — kept separate from the dialog's open
  // state so "no project preselected" isn't confused with "closed".
  const [createWorktreeDialogOpen, setCreateWorktreeDialogOpen] = useState(false);
  const [createWorktreeDialogProjectRef, setCreateWorktreeDialogProjectRef] =
    useState<ScopedProjectRef | null>(null);
  const openCreateWorktreeDialog = useCallback((projectRef: ScopedProjectRef | null = null) => {
    setCreateWorktreeDialogProjectRef(projectRef);
    setCreateWorktreeDialogOpen(true);
  }, []);
  const {
    archiveThread,
    deleteThread,
    worktreeRemovalTarget,
    requestWorktreeRemoval,
    closeWorktreeRemovalDialog,
    completeWorktreeRemoval,
  } = useThreadActions();
  const handleWorktreeRemoved = useCallback(
    async (removedTarget: WorktreeRemovalTarget, result: WorktreeRemovalResult) => {
      const cleanupResult = await completeWorktreeRemoval(removedTarget, result);
      if (cleanupResult._tag !== "Failure" || isAtomCommandInterrupted(cleanupResult)) return;
      const error = squashAtomCommandFailure(cleanupResult);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Worktree removed, but navigation failed",
          description:
            error instanceof Error
              ? error.message
              : "Select another thread from the sidebar to continue.",
        }),
      );
    },
    [completeWorktreeRemoval],
  );
  const { isMobile, setOpenMobile } = useSidebar();
  const routeThreadRef = useParams({
    strict: false,
    select: (params) => resolveThreadRouteRef(params),
  });
  const routeThreadKey = routeThreadRef ? scopedThreadKey(routeThreadRef) : null;
  const routeProjectScopedKey = useParams({
    strict: false,
    select: (params) => {
      const ref = resolveProjectRouteRef(params);
      return ref === null ? null : scopedProjectKey(ref);
    },
  });
  const routeTerminalOpen = useThreadHasTerminalSurface(routeThreadRef);
  const keybindings = useAtomValue(primaryServerKeybindingsAtom);
  const openAddProjectCommandPalette = useOpenAddProjectCommandPalette();
  const [expandedThreadListsByProject, setExpandedThreadListsByProject] = useState<
    ReadonlySet<string>
  >(() => new Set());
  const { showThreadJumpHints, updateThreadJumpHintsVisibility } = useThreadJumpHintVisibility();
  const dragInProgressRef = useRef(false);
  const suppressProjectClickAfterDragRef = useRef(false);
  const suppressProjectClickForContextMenuRef = useRef(false);
  const desktopUpdateState = useDesktopUpdateState();
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const projectAvailability = useMemo(
    () =>
      resolveSidebarProjectAvailability({
        projectCount: projects.length,
        catalogReady: shellSummary.catalogReady,
        catalogHealth: shellSummary.catalogHealth,
        environments: shellSummary.statuses,
      }),
    [projects.length, shellSummary.catalogHealth, shellSummary.catalogReady, shellSummary.statuses],
  );
  const projectAvailabilityEnvironment = useMemo(() => {
    if (projectAvailability.environmentId === null) {
      return null;
    }
    return (
      environments.find(
        (environment) => environment.environmentId === projectAvailability.environmentId,
      ) ?? null
    );
  }, [environments, projectAvailability.environmentId]);
  const projectAvailabilityTarget = projectAvailabilityEnvironment?.entry.target ?? null;
  const projectAvailabilityActions = environmentConnectionActions(
    presentation,
    projectAvailabilityTarget,
  );
  const showProjectAvailabilityRetry = projectAvailabilityActions.reconnect;
  const showProjectAvailabilityOpenRemoteServers = projectAvailabilityActions.openRemoteServers;
  const showProjectAvailabilityConnectionSettings =
    presentation.showRemoteDeviceControls ||
    (presentation.showLocalEnvironmentSettings &&
      projectAvailabilityTarget !== null &&
      presentation.presentsTarget(projectAvailabilityTarget));
  const handleRetryProjectEnvironment = useCallback(
    (environmentId: EnvironmentId) => {
      void retryProjectEnvironment(environmentId);
    },
    [retryProjectEnvironment],
  );
  const handleOpenProjectSettings = useCallback(() => {
    void navigate({ to: "/settings/remote-servers" });
  }, [navigate]);
  const handleViewProjectDiagnostics = useCallback(() => {
    void navigate({ to: "/settings/diagnostics" });
  }, [navigate]);
  const handleRecoverProjectData = useCallback((environmentId: EnvironmentId) => {
    void projectDataSafetyStore.open(environmentId, "manual").catch(() => undefined);
  }, []);
  const handleAdoptProjectStorage = useCallback(
    (environmentId: EnvironmentId) => {
      const api = readLocalApi();
      if (!api) {
        return;
      }
      void api.dialogs
        .confirm(
          "Use this project data location? Projects from the two locations will not be merged.",
        )
        .then((confirmed) => {
          if (confirmed) {
            void adoptProjectStorage(environmentId);
          }
        })
        .catch(() => undefined);
    },
    [adoptProjectStorage],
  );
  const clearSelection = useThreadSelectionStore((s) => s.clearSelection);
  const setSelectionAnchor = useThreadSelectionStore((s) => s.setAnchor);
  const platform = navigator.platform;
  const shortcutModifiers = useShortcutModifierState();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const environmentLabelById = useMemo(
    () =>
      new Map(
        environments.map((environment) => [environment.environmentId, environment.label] as const),
      ),
    [environments],
  );
  const desktopLocalEnvironmentIds = useMemo(
    () =>
      new Set(
        environments
          .filter((environment) => isDesktopLocalConnectionTarget(environment.entry.target))
          .map((environment) => environment.environmentId),
      ),
    [environments],
  );
  const projectDataRecoveryEnvironmentIds = useMemo(
    () =>
      new Set(
        environments
          .filter(
            (environment) =>
              isDesktopHost &&
              typeof window !== "undefined" &&
              window.desktopBridge?.getProjectDataStatuses !== undefined &&
              (environment.entry.target._tag === "PrimaryConnectionTarget" ||
                isDesktopLocalConnectionTarget(environment.entry.target)),
          )
          .map((environment) => environment.environmentId),
      ),
    [environments],
  );
  const orderedProjects = useMemo(() => {
    return orderItemsByPreferredIds({
      items: projects,
      preferredIds: projectOrder,
      getId: getProjectOrderKey,
      getPreferenceIds: (project) => [
        getProjectOrderKey(project),
        legacyProjectCwdPreferenceKey(project.workspaceRoot),
      ],
    });
  }, [projectOrder, projects]);

  // Build a mapping from physical project key → logical project key for
  // cross-environment grouping.  Projects that share a repositoryIdentity
  // canonicalKey are treated as one logical project in the sidebar.
  const physicalToLogicalKey = useMemo(() => {
    return buildPhysicalToLogicalProjectKeyMap({
      projects: orderedProjects,
      settings: effectiveGroupingSettings,
    });
  }, [orderedProjects, effectiveGroupingSettings]);
  const projectPhysicalKeyByScopedRef = useMemo(
    () =>
      new Map(
        orderedProjects.map((project) => [
          scopedProjectKey(scopeProjectRef(project.environmentId, project.id)),
          derivePhysicalProjectKey(project),
        ]),
      ),
    [orderedProjects],
  );

  const sidebarProjects = useMemo<SidebarProjectSnapshot[]>(() => {
    const snapshots = buildSidebarProjectSnapshots({
      projects: orderedProjects,
      settings: effectiveGroupingSettings,
      primaryEnvironmentId,
      resolveEnvironmentLabel: (environmentId) => environmentLabelById.get(environmentId) ?? null,
      isDesktopLocalEnvironment: (environmentId) => desktopLocalEnvironmentIds.has(environmentId),
    });
    // A card shares expansion with the row that holds it in the Environments view.
    return repositoriesView
      ? snapshots.map((snapshot) => ({
          ...snapshot,
          sharedExpansionKey: deriveLogicalProjectKeyFromSettings(
            snapshot,
            projectGroupingSettings,
          ),
        }))
      : snapshots;
  }, [
    environmentLabelById,
    desktopLocalEnvironmentIds,
    effectiveGroupingSettings,
    orderedProjects,
    projectGroupingSettings,
    primaryEnvironmentId,
    repositoriesView,
  ]);

  const sidebarProjectByKey = useMemo(
    () => new Map(sidebarProjects.map((project) => [project.projectKey, project] as const)),
    [sidebarProjects],
  );
  const sidebarThreadByKey = useMemo(
    () =>
      new Map(
        sidebarThreads.map(
          (thread) =>
            [scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)), thread] as const,
        ),
      ),
    [sidebarThreads],
  );
  // Keep the routed thread's physical project ID for project-scoped actions,
  // and resolve its logical key separately for grouped sidebar highlighting.
  const activeRouteProject = useMemo(() => {
    if (!routeThreadKey) {
      return null;
    }
    const activeThread = sidebarThreadByKey.get(routeThreadKey);
    if (!activeThread) return null;
    const physicalKey =
      projectPhysicalKeyByScopedRef.get(
        scopedProjectKey(scopeProjectRef(activeThread.environmentId, activeThread.projectId)),
      ) ?? scopedProjectKey(scopeProjectRef(activeThread.environmentId, activeThread.projectId));
    return {
      projectRef: scopeProjectRef(activeThread.environmentId, activeThread.projectId),
      projectKey: physicalToLogicalKey.get(physicalKey) ?? physicalKey,
    };
  }, [routeThreadKey, sidebarThreadByKey, physicalToLogicalKey, projectPhysicalKeyByScopedRef]);
  const activeRouteProjectRef = activeRouteProject?.projectRef ?? null;
  const activeRouteProjectKey = activeRouteProject?.projectKey ?? null;
  // A clicked project header stays the selected node until the routed thread
  // changes; the store remembers the route it was selected under.
  const selectedProjectKey = useThreadSelectionStore((state) =>
    state.selectedProjectRouteThreadKey === routeThreadKey ? state.selectedProjectKey : null,
  );
  const selectProject = useCallback(
    (projectKey: string) => {
      useThreadSelectionStore.getState().selectProject(projectKey, routeThreadKey);
    },
    [routeThreadKey],
  );
  // Project modules highlight the same physical or grouped project header.
  const moduleRouteProjectKey = useMemo(
    () =>
      projectModuleRouteProjectKey(
        pathname,
        routeProjectScopedKey,
        projectPhysicalKeyByScopedRef,
        physicalToLogicalKey,
      ),
    [pathname, physicalToLogicalKey, projectPhysicalKeyByScopedRef, routeProjectScopedKey],
  );

  // Group threads by logical project key so all threads from grouped projects
  // are displayed together.
  const threadsByProjectKey = useMemo(() => {
    const next = new Map<string, SidebarThreadSummary[]>();
    for (const thread of sidebarThreads) {
      const physicalKey =
        projectPhysicalKeyByScopedRef.get(
          scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId)),
        ) ?? scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId));
      const logicalKey = physicalToLogicalKey.get(physicalKey) ?? physicalKey;
      const existing = next.get(logicalKey);
      if (existing) {
        existing.push(thread);
      } else {
        next.set(logicalKey, [thread]);
      }
    }
    return next;
  }, [sidebarThreads, physicalToLogicalKey, projectPhysicalKeyByScopedRef]);
  const getCurrentSidebarShortcutContext = useCallback(
    () => ({
      terminalFocus: isTerminalFocused(),
      terminalOpen: routeTerminalOpen,
      modelPickerOpen: isModelPickerOpen(),
    }),
    [routeTerminalOpen],
  );
  const newThreadShortcutLabelOptions = useMemo(
    () => ({
      platform,
      context: {
        terminalFocus: false,
        terminalOpen: false,
      },
    }),
    [platform],
  );
  const navigateToThread = useCallback(
    (threadRef: ScopedThreadRef) => {
      if (useThreadSelectionStore.getState().selectedThreadKeys.size > 0) {
        clearSelection();
      }
      setSelectionAnchor(scopedThreadKey(threadRef));
      if (isMobile) {
        setOpenMobile(false);
      }
      void navigate({
        to: "/$environmentId/$threadId",
        params: buildThreadRouteParams(threadRef),
      });
    },
    [clearSelection, isMobile, navigate, setOpenMobile, setSelectionAnchor],
  );

  const projectDnDSensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: { distance: 6 },
    }),
  );
  const projectCollisionDetection = useCallback<CollisionDetection>((args) => {
    const pointerCollisions = pointerWithin(args);
    if (pointerCollisions.length > 0) {
      return pointerCollisions;
    }

    return closestCorners(args);
  }, []);

  const handleProjectDragEnd = useCallback(
    (event: DragEndEvent) => {
      if (sidebarProjectSortOrder !== "manual") {
        dragInProgressRef.current = false;
        return;
      }
      dragInProgressRef.current = false;
      const { active, over } = event;
      if (!over || active.id === over.id) return;
      const activeProject = sidebarProjects.find((project) => project.projectKey === active.id);
      const overProject = sidebarProjects.find((project) => project.projectKey === over.id);
      if (!activeProject || !overProject) return;
      const activeMemberKeys = activeProject.memberProjects.map(
        (member) => member.physicalProjectKey,
      );
      const overMemberKeys = overProject.memberProjects.map((member) => member.physicalProjectKey);
      reorderProjects(orderedProjects.map(getProjectOrderKey), activeMemberKeys, overMemberKeys);
    },
    [orderedProjects, sidebarProjectSortOrder, reorderProjects, sidebarProjects],
  );

  const handleProjectDragStart = useCallback(
    (_event: DragStartEvent) => {
      if (sidebarProjectSortOrder !== "manual") {
        return;
      }
      dragInProgressRef.current = true;
      suppressProjectClickAfterDragRef.current = true;
    },
    [sidebarProjectSortOrder],
  );

  const handleProjectDragCancel = useCallback((_event: DragCancelEvent) => {
    dragInProgressRef.current = false;
  }, []);

  const animatedProjectListsRef = useRef(new WeakSet<HTMLElement>());
  const attachProjectListAutoAnimateRef = useCallback((node: HTMLElement | null) => {
    if (!node || animatedProjectListsRef.current.has(node)) {
      return;
    }
    autoAnimate(node, SIDEBAR_LIST_ANIMATION_OPTIONS);
    animatedProjectListsRef.current.add(node);
  }, []);

  const animatedThreadListsRef = useRef(new WeakSet<HTMLElement>());
  const attachThreadListAutoAnimateRef = useCallback((node: HTMLElement | null) => {
    if (!node || animatedThreadListsRef.current.has(node)) {
      return;
    }
    autoAnimate(node, SIDEBAR_LIST_ANIMATION_OPTIONS);
    animatedThreadListsRef.current.add(node);
  }, []);

  const visibleThreads = useMemo(
    () => sidebarThreads.filter((thread) => thread.archivedAt === null),
    [sidebarThreads],
  );
  const sortedProjects = useMemo(() => {
    const sortableProjects = sidebarProjects.map((project) => ({
      ...project,
      id: project.projectKey,
    }));
    const sortableThreads = visibleThreads.map((thread) => {
      const physicalKey =
        projectPhysicalKeyByScopedRef.get(
          scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId)),
        ) ?? scopedProjectKey(scopeProjectRef(thread.environmentId, thread.projectId));
      return {
        ...thread,
        projectId: (physicalToLogicalKey.get(physicalKey) ?? physicalKey) as ProjectId,
      };
    });
    return sortProjectsForSidebar(
      sortableProjects,
      sortableThreads,
      effectiveProjectSortOrder,
    ).flatMap((project) => {
      const resolvedProject = sidebarProjectByKey.get(project.id);
      return resolvedProject ? [resolvedProject] : [];
    });
  }, [
    effectiveProjectSortOrder,
    physicalToLogicalKey,
    projectPhysicalKeyByScopedRef,
    sidebarProjectByKey,
    sidebarProjects,
    visibleThreads,
  ]);
  // Built and subscribed only in the Repositories view, so connection-phase changes do not
  // re-render the Environments view's project list.
  const environmentCardIdentities = useMemo(
    () =>
      repositoriesView
        ? buildEnvironmentCardIdentities(
            environments.map((environment) =>
              toEnvironmentRailCandidate({
                environmentId: environment.environmentId,
                label: environment.label,
                target: environment.entry.target,
                phase: environment.connection.phase,
                compat: resolveEnvironmentCompatVerdict(environment.serverConfig),
                updateAvailable: false,
              }),
            ),
          )
        : NO_ENVIRONMENT_CARD_IDENTITIES,
    [environments, repositoriesView],
  );
  const repositoryGroupExpandedById = useUiStateStore((store) =>
    repositoriesView ? store.repositoryGroupExpandedById : NO_REPOSITORY_GROUP_EXPANSION,
  );
  const repositoryGroups = useMemo(
    () =>
      repositoriesView
        ? groupProjectsByRepository({
            projects: sortedProjects,
            environments: environmentCardIdentities,
            sortOrder: sidebarRepositorySortOrder,
          })
        : null,
    [environmentCardIdentities, repositoriesView, sidebarRepositorySortOrder, sortedProjects],
  );
  // Projects in the order they appear on screen, so jump labels follow the current view.
  const visibleProjectsInOrder = useMemo(
    () =>
      repositoryGroups === null
        ? sortedProjects
        : repositoryGroups.flatMap((group) =>
            repositoryGroupExpandedById[group.key] === false ? [] : group.cards,
          ),
    [repositoryGroupExpandedById, repositoryGroups, sortedProjects],
  );
  const isManualProjectSorting = sidebarProjectSortOrder === "manual";
  const visibleSidebarThreadKeys = useMemo(
    () =>
      visibleProjectsInOrder.flatMap((project) => {
        const projectThreads = sortThreads(
          (threadsByProjectKey.get(project.projectKey) ?? []).filter(
            (thread) => thread.archivedAt === null,
          ),
          sidebarThreadSortOrder,
        );
        const projectExpanded = resolveProjectExpanded(
          projectExpandedById,
          projectExpansionPreferenceKeys(project),
        );
        const activeThreadKey = routeThreadKey ?? undefined;
        const pinnedCollapsedThread =
          !projectExpanded && activeThreadKey
            ? (projectThreads.find(
                (thread) =>
                  scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)) ===
                  activeThreadKey,
              ) ?? null)
            : null;
        const shouldShowThreadPanel = projectExpanded || pinnedCollapsedThread !== null;
        if (!shouldShowThreadPanel) {
          return [];
        }
        const isThreadListExpanded = expandedThreadListsByProject.has(project.projectKey);
        const hasOverflowingThreads = projectThreads.length > sidebarThreadPreviewCount;
        const previewThreads =
          isThreadListExpanded || !hasOverflowingThreads
            ? projectThreads
            : projectThreads.slice(0, sidebarThreadPreviewCount);
        const renderedThreads = pinnedCollapsedThread ? [pinnedCollapsedThread] : previewThreads;
        return renderedThreads.map((thread) =>
          scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
        );
      }),
    [
      sidebarThreadSortOrder,
      sidebarThreadPreviewCount,
      expandedThreadListsByProject,
      projectExpandedById,
      routeThreadKey,
      threadsByProjectKey,
      visibleProjectsInOrder,
    ],
  );
  const threadJumpCommandByKey = useMemo(() => {
    const mapping = new Map<string, NonNullable<ReturnType<typeof threadJumpCommandForIndex>>>();
    for (const [visibleThreadIndex, threadKey] of visibleSidebarThreadKeys.entries()) {
      const jumpCommand = threadJumpCommandForIndex(visibleThreadIndex);
      if (!jumpCommand) {
        return mapping;
      }
      mapping.set(threadKey, jumpCommand);
    }

    return mapping;
  }, [visibleSidebarThreadKeys]);
  const threadJumpThreadKeys = useMemo(
    () => [...threadJumpCommandByKey.keys()],
    [threadJumpCommandByKey],
  );
  const threadJumpLabelByKey = useMemo(
    () =>
      buildThreadJumpLabelMap({
        keybindings,
        platform,
        terminalOpen: routeTerminalOpen,
        threadJumpCommandByKey,
      }),
    [keybindings, platform, routeTerminalOpen, threadJumpCommandByKey],
  );
  const visibleThreadJumpLabelByKey = showThreadJumpHints
    ? threadJumpLabelByKey
    : EMPTY_THREAD_JUMP_LABELS;
  const orderedSidebarThreadKeys = visibleSidebarThreadKeys;
  const prewarmedSidebarThreadKeys = useMemo(
    () => getSidebarThreadIdsToPrewarm(visibleSidebarThreadKeys),
    [visibleSidebarThreadKeys],
  );
  const prewarmedSidebarThreadRefs = useMemo(
    () =>
      prewarmedSidebarThreadKeys.flatMap((threadKey) => {
        const ref = parseScopedThreadKey(threadKey);
        return ref ? [ref] : [];
      }),
    [prewarmedSidebarThreadKeys],
  );

  // The model picker's state lives in the DOM, so it is read here on every modifier change:
  // read while rendering, the compiled component would keep its first answer.
  useEffect(() => {
    updateThreadJumpHintsVisibility(
      shouldShowThreadJumpHintsForModifiers(shortcutModifiers, keybindings, {
        platform,
        context: {
          terminalFocus: false,
          terminalOpen: routeTerminalOpen,
          modelPickerOpen: isModelPickerOpen(),
        },
      }),
    );
  }, [
    keybindings,
    platform,
    routeTerminalOpen,
    shortcutModifiers,
    updateThreadJumpHintsVisibility,
  ]);

  useEffect(() => {
    const onWindowKeyDown = (event: globalThis.KeyboardEvent) => {
      handleSidebarNavigationKeyDown({
        event,
        resolveCommand: () =>
          resolveShortcutCommand(event, keybindings, {
            platform,
            context: getCurrentSidebarShortcutContext(),
          }),
        orderedThreadKeys: orderedSidebarThreadKeys,
        currentThreadKey: routeThreadKey,
        jumpThreadKeys: threadJumpThreadKeys,
        threadByKey: sidebarThreadByKey,
        navigateToThread,
      });
    };

    window.addEventListener("keydown", onWindowKeyDown);

    return () => {
      window.removeEventListener("keydown", onWindowKeyDown);
    };
  }, [
    getCurrentSidebarShortcutContext,
    keybindings,
    navigateToThread,
    orderedSidebarThreadKeys,
    platform,
    routeThreadKey,
    sidebarThreadByKey,
    threadJumpThreadKeys,
  ]);

  useEffect(() => {
    const onMouseDown = (event: globalThis.MouseEvent) => {
      handleSidebarSelectionMouseDown({
        hasSelection: useThreadSelectionStore.getState().hasSelection(),
        target: event.target,
        clearSelection,
      });
    };

    window.addEventListener("mousedown", onMouseDown);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
    };
  }, [clearSelection]);

  const desktopUpdateButtonDisabled = isDesktopUpdateButtonDisabled(desktopUpdateState);
  const desktopUpdateButtonAction = desktopUpdateState
    ? resolveDesktopUpdateButtonAction(desktopUpdateState)
    : "none";
  const showArm64IntelBuildWarning =
    isDesktopHost && shouldShowArm64IntelBuildWarning(desktopUpdateState);
  const arm64IntelBuildWarningDescription =
    desktopUpdateState && showArm64IntelBuildWarning
      ? getArm64IntelBuildWarningDescription(desktopUpdateState)
      : null;
  const commandPaletteShortcutLabel = shortcutLabelForCommand(
    keybindings,
    "commandPalette.toggle",
    newThreadShortcutLabelOptions,
  );
  const handleDesktopUpdateButtonClick = useCallback(() => {
    const bridge = window.desktopBridge;
    if (!bridge || !desktopUpdateState) return;
    if (desktopUpdateButtonDisabled || desktopUpdateButtonAction === "none") return;

    if (desktopUpdateButtonAction === "download") {
      void bridge
        .downloadUpdate()
        .then((result) => {
          if (result.completed) {
            toastManager.add({
              type: "success",
              title: "Update downloaded",
              description: "Restart the app from the update button to install it.",
            });
          }
          if (!shouldToastDesktopUpdateActionResult(result)) return;
          const actionError = getDesktopUpdateActionError(result);
          if (!actionError) return;
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Could not download update",
              description: actionError,
            }),
          );
        })
        .catch((error) => {
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title: "Could not start update download",
              description: error instanceof Error ? error.message : "An unexpected error occurred.",
            }),
          );
        });
      return;
    }

    if (desktopUpdateButtonAction === "install") {
      setUpdateDialogOpen(true);
    }
  }, [desktopUpdateButtonAction, desktopUpdateButtonDisabled, desktopUpdateState]);

  const expandThreadListForProject = useCallback((projectKey: string) => {
    setExpandedThreadListsByProject((current) => {
      if (current.has(projectKey)) return current;
      const next = new Set(current);
      next.add(projectKey);
      return next;
    });
  }, []);

  const collapseThreadListForProject = useCallback((projectKey: string) => {
    setExpandedThreadListsByProject((current) => {
      if (!current.has(projectKey)) return current;
      const next = new Set(current);
      next.delete(projectKey);
      return next;
    });
  }, []);

  return (
    <WorktreeRemovalRequestContext.Provider value={requestWorktreeRemoval}>
      <CreateWorktreeDialog
        open={createWorktreeDialogOpen}
        onOpenChange={(open) => {
          setCreateWorktreeDialogOpen(open);
          if (!open) setCreateWorktreeDialogProjectRef(null);
        }}
        defaultProjectRef={createWorktreeDialogProjectRef ?? activeRouteProjectRef}
      />
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
      {desktopUpdateState && window.desktopBridge ? (
        <UpdateProtectionDialog
          open={updateDialogOpen}
          state={desktopUpdateState}
          onOpenChange={setUpdateDialogOpen}
          installUpdate={(input) => window.desktopBridge!.installUpdate(input)}
          onDiagnostics={() => {
            setUpdateDialogOpen(false);
            handleViewProjectDiagnostics();
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
      {prewarmedSidebarThreadRefs.map((threadRef) => (
        <SidebarThreadDetailPrewarmer key={scopedThreadKey(threadRef)} threadRef={threadRef} />
      ))}
      <SidebarChromeHeader />

      {isOnSettings ? (
        <SettingsSidebarNav pathname={pathname} />
      ) : (
        <>
          <SidebarViewToggle />
          {repositoriesView ? null : <SidebarEnvironmentContextCard />}
          <SidebarProjectsContent
            showArm64IntelBuildWarning={showArm64IntelBuildWarning}
            arm64IntelBuildWarningDescription={arm64IntelBuildWarningDescription}
            desktopUpdateButtonAction={desktopUpdateButtonAction}
            desktopUpdateButtonDisabled={desktopUpdateButtonDisabled}
            handleDesktopUpdateButtonClick={handleDesktopUpdateButtonClick}
            projectSortOrder={sidebarProjectSortOrder}
            repositorySortOrder={sidebarRepositorySortOrder}
            threadSortOrder={sidebarThreadSortOrder}
            projectGroupingMode={sidebarProjectGroupingMode}
            threadPreviewCount={sidebarThreadPreviewCount}
            updateSettings={updateSettings}
            openAddProject={openAddProjectCommandPalette}
            addProjectLabel={addProjectLabel}
            isManualProjectSorting={isManualProjectSorting}
            projectDnDSensors={projectDnDSensors}
            projectCollisionDetection={projectCollisionDetection}
            handleProjectDragStart={handleProjectDragStart}
            handleProjectDragEnd={handleProjectDragEnd}
            handleProjectDragCancel={handleProjectDragCancel}
            openCreateWorktreeDialog={openCreateWorktreeDialog}
            archiveThread={archiveThread}
            deleteThread={deleteThread}
            sortedProjects={sortedProjects}
            repositoriesView={repositoriesView}
            repositoryGroups={repositoryGroups}
            repositoryGroupExpandedById={repositoryGroupExpandedById}
            environmentCardIdentities={environmentCardIdentities}
            expandedThreadListsByProject={expandedThreadListsByProject}
            activeRouteProjectKey={activeRouteProjectKey}
            routeThreadKey={routeThreadKey}
            moduleRouteProjectKey={moduleRouteProjectKey}
            selectedProjectKey={selectedProjectKey}
            selectProject={selectProject}
            commandPaletteShortcutLabel={commandPaletteShortcutLabel}
            threadJumpLabelByKey={visibleThreadJumpLabelByKey}
            attachThreadListAutoAnimateRef={attachThreadListAutoAnimateRef}
            expandThreadListForProject={expandThreadListForProject}
            collapseThreadListForProject={collapseThreadListForProject}
            dragInProgressRef={dragInProgressRef}
            suppressProjectClickAfterDragRef={suppressProjectClickAfterDragRef}
            suppressProjectClickForContextMenuRef={suppressProjectClickForContextMenuRef}
            attachProjectListAutoAnimateRef={attachProjectListAutoAnimateRef}
            projectAvailability={projectAvailability}
            projectAvailabilityEnvironment={projectAvailabilityEnvironment}
            showProjectAvailabilityRetry={showProjectAvailabilityRetry}
            showProjectAvailabilityConnectionSettings={showProjectAvailabilityConnectionSettings}
            showProjectAvailabilityOpenRemoteServers={showProjectAvailabilityOpenRemoteServers}
            onRetryProjectEnvironment={handleRetryProjectEnvironment}
            onOpenProjectSettings={handleOpenProjectSettings}
            onViewProjectDiagnostics={handleViewProjectDiagnostics}
            onAdoptProjectStorage={handleAdoptProjectStorage}
            onRecoverProjectData={
              projectAvailability.environmentId !== null &&
              projectDataRecoveryEnvironmentIds.has(projectAvailability.environmentId)
                ? handleRecoverProjectData
                : undefined
            }
          />

          <SidebarSeparator />
          <SidebarChromeFooter />
        </>
      )}
    </WorktreeRemovalRequestContext.Provider>
  );
}
