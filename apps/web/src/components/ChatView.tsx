import { attachmentAdmissionOwner } from "../state/attachmentAdmissions";
import {
  ActivityError,
  type ActivityRecordId,
  type ActivityRecordKind,
  type ActivityScopeRef,
  type ActivitySection,
  type ApprovalRequestId,
  DEFAULT_MODEL,
  defaultInstanceIdForDriver,
  type EnvironmentId,
  type MessageId,
  type ModelSelection,
  type ProjectScript,
  type ProjectId,
  type ProviderApprovalDecision,
  ProviderInstanceId,
  type ServerProvider,
  type ResolvedKeybindingsConfig,
  type ScopedThreadRef,
  type ThreadId,
  type TurnId,
  type KeybindingCommand,
  OrchestrationThreadActivity,
  ProviderInteractionMode,
  ProviderDriverKind,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
  RuntimeMode,
  UploadError,
  RpcResponseTooLargeError,
} from "@bibcode/contracts";
import {
  describeUnavailableEnvironment,
  selectQueuedMessages,
  deriveQueuedCardStatus,
  deliveryOffersRetry,
  deliveryProviderLabel,
  findBlockingDelivery,
  waitsBehind,
  shouldEnqueueOnSend,
  isQueuedTimelineMessage,
} from "./ChatView.logic";
import { queuedMessageCache } from "./chat/queuedMessageCache";
import { mergeQueuedMessageIntoDraft } from "./chat/restoreQueuedMessage";
import {
  attachmentUploadFailureCopy,
  type PendingAttachmentUpload,
} from "./chat/AttachmentUploadNotice";
import {
  UploadClientError,
  type StartThreadTurnInput,
  type AttachmentAdmissionAuthority,
} from "@bibcode/client-runtime/operations";
import { RpcClientError } from "effect/rpc";
import { EnvironmentRpcUnavailableError } from "@bibcode/client-runtime/rpc";
import {
  prepareTurnAttachments,
  mergeInterruptedComposerContent,
  reusableStagedAttachmentAttempt,
} from "./ChatView.logic";
import type { TimestampFormat } from "@bibcode/contracts/settings";
import {
  type EnvironmentConnectionPresentation,
  isConnectionUnavailable,
} from "@bibcode/client-runtime/connection";
import {
  scopedProjectKey,
  scopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@bibcode/client-runtime/environment";
import { applyClaudePromptEffortPrefix, resolvePromptInjectedEffort } from "@bibcode/shared/model";
import { canonicalizeLegacyComposerFileReferences } from "@bibcode/shared/composerReferences";
import { resolveProviderSessionDefault } from "@bibcode/shared/providerSessionDefaults";
import { CHAT_LIST_ANCHOR_OFFSET } from "@bibcode/shared/chatList";
import { projectScriptCwd, projectScriptRuntimeEnv } from "@bibcode/shared/projectScripts";
import { truncate } from "@bibcode/shared/String";
import { resolveTerminalSessionLabel } from "@bibcode/shared/terminalLabels";
import { Debouncer } from "@tanstack/react-pacer";
import { useAtomRefresh, useAtomValue } from "@effect/atom-react";
import {
  lazy,
  memo,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
  type ReactNode,
  useSyncExternalStore,
} from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  isAtomCommandInterrupted,
  mapAtomCommandResult,
  settlePromise,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@bibcode/client-runtime/state/runtime";
import {
  selectWorktreeCatalogCapabilityPolicy,
  selectWorktreeWorkspaceActionsAvailable,
} from "@bibcode/client-runtime/state/worktrees";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { AsyncResult, Atom } from "effect/reactivity";
import {
  EMPTY_ENVIRONMENT_ACTIVITY_STATE,
  type EnvironmentActivityState,
} from "@bibcode/client-runtime/state/activity";
import { isDesktopHost } from "../env";
import { readLocalApi } from "../localApi";
import { useDiffPanelStore } from "../diffPanelStore";
import { selectActivityDockExpanded, useActivityDockStore } from "../activityDockStore";
import { parseStandaloneComposerBiBCodeAction } from "../composer-logic";
import {
  derivePendingApprovals,
  derivePendingUserInputs,
  derivePhase,
  deriveTimelineEntries,
  deriveActiveWorkStartedAt,
  deriveActivePlanState,
  findSidebarProposedPlan,
  findLatestProposedPlan,
  deriveWorkLogEntries,
  hasActionableProposedPlan,
  isLatestTurnSettled,
} from "../session-logic";
import { type LegendListRef } from "@legendapp/list/react";
import { getAnchoredTurnMetrics, type TimelineScrollMode } from "./chat/timelineScrollAnchoring";
import {
  buildPendingUserInputAnswers,
  derivePendingUserInputProgress,
  setPendingUserInputCustomAnswer,
  togglePendingUserInputOptionSelection,
  type PendingUserInputDraftAnswer,
} from "../pendingUserInput";
import { useUiStateStore } from "../uiStateStore";
import {
  buildPlanImplementationThreadTitle,
  buildPlanImplementationPrompt,
  resolvePlanFollowUpSubmission,
} from "../proposedPlan";
import {
  DEFAULT_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  MAX_TERMINALS_PER_GROUP,
  type ChatMessage,
  type SessionPhase,
  type Thread,
  type TurnDeliveryResolutionAction,
  type TurnDiffSummary,
} from "../types";
import { useTheme } from "../hooks/useTheme";
import {
  mergeTerminalSpawnEnv,
  resolveTerminalThemeMode,
  usesPersistentWindowsConsoleTheme,
  type TerminalThemeMode,
} from "./terminalTheme";
import { useTurnDiffSummaries } from "../hooks/useTurnDiffSummaries";
import { isCommandPaletteOpen } from "../commandPaletteContext";
import { buildTemporaryWorktreeBranchName } from "@bibcode/shared/git";
import { useMediaQuery } from "../hooks/useMediaQuery";
import { resolveProviderSessionSelectionForInstance } from "../providerSessionSelection";
import { formatProviderSlugLabel } from "../providerModels";
import {
  ACTIVITY_DOCK_COMPACT_MEDIA_QUERY,
  RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY,
} from "../rightPanelLayout";
import {
  selectActiveRightPanel,
  selectActiveRightPanelSurface,
  selectThreadRightPanelState,
  type ActivityRightPanelSurface,
  type RightPanelSurface,
  useRightPanelStore,
} from "../rightPanelStore";
import {
  type CenterSurface,
  type CenterTerminalPlacement,
  selectFocusedCenterSurface,
  selectFocusedCenterPanelGroup,
  selectThreadCenterPanelState,
  type ThreadCenterPanelState,
  type OpenTerminalPanelOptions,
  useCenterPanelStore,
} from "../centerPanelStore";
import {
  findCenterPanelGroupEdges,
  type CenterPanelDropRequest,
  type CenterPanelLayoutPath,
} from "../centerPanelLayout";
import { useCenterPanelActions } from "../centerPanelActions";
import {
  applyProviderInstanceSettings,
  deriveProviderInstanceEntries,
  providerDriverLabel,
  type ProviderInstanceEntry,
} from "../providerInstances";
import {
  CenterPanelWorkspace,
  type CenterPanelWorkspaceHandle,
  type CenterPanelWorkspaceProps,
} from "./CenterPanelWorkspace";
import {
  createCenterTerminal,
  type CenterTerminalCreationResult,
  type CenterTerminalLaunch,
  type CenterTerminalSessionCommandResult,
} from "../centerTerminalActions";
import { reserveTerminalId, type TerminalIdReservation } from "../terminalIdReservations";
import { retireTerminalSession, type TerminalRetirementTarget } from "../terminalRetirement";
import type { CenterPanelSurfaceRenderContext } from "./CenterPanelSurfaceHosts";
import { CenterTerminalPanel } from "./CenterTerminalPanel";
import {
  isPreviewSupportedInRuntime,
  setActivePreviewTab,
  useThreadPreviewState,
} from "../previewStateStore";
import { addBrowserSurface } from "./preview/addBrowserSurface";
import { closePreviewSession } from "./preview/closePreviewSession";
import { subscribePreviewAction } from "./preview/previewActionBus";
import { getConfiguredPreviewUrls } from "./preview/previewEmptyStateLogic";
import { RightPanelTabs } from "./RightPanelTabs";
import { useDevicePreviewsAvailability } from "./device/deviceAvailability";
import { DiffWorkerPoolProvider } from "./DiffWorkerPoolProvider";
import { resolveShortcutCommand, shortcutLabelForCommand } from "../keybindings";
import PlanSidebar from "./PlanSidebar";
import ThreadTerminalPanel, {
  enqueueTerminalInput,
  releaseTerminalUiResources,
} from "./ThreadTerminalPanel";
import { ChevronDownIcon, TriangleAlertIcon, WifiOffIcon } from "lucide-react";
import { cn, randomHex } from "~/lib/utils";
import { stackedThreadToast, toastManager } from "./ui/toast";
import { decodeProjectScriptKeybindingRule } from "~/lib/projectScriptKeybindings";
import { type NewProjectScriptInput } from "./ProjectScriptsControl";
import {
  commandForProjectScript,
  nextProjectScriptId,
  projectScriptIdFromCommand,
} from "~/projectScripts";
import { newCommandId, newDraftId, newMessageId, newThreadId } from "~/lib/utils";
import { getProviderModelCapabilities, resolveSelectableProvider } from "../providerModels";
import { useEnvironmentSettings } from "../hooks/useSettings";
import { readTerminalFittedSize } from "./terminalSizing";
import { worktreeEnvironment } from "../state/worktrees";
import { resolveAppModelSelectionForInstance } from "../modelSelection";
import { resolveThreadProviderBinding } from "../threadProviderBinding";
import { getTerminalFocusOwner } from "../lib/terminalFocus";
import { DesktopPreviewTabHosts } from "../browser/DesktopPreviewTabHosts";
import {
  deriveLogicalProjectKeyFromSettings,
  projectDraftFallbackKeys,
  selectProjectGroupingSettings,
} from "../logicalProject";
import { buildDraftThreadRouteParams } from "../threadRoutes";
import {
  type ComposerAttachment,
  type DraftThreadEnvMode,
  useComposerDraftStore,
  type DraftId,
} from "../composerDraftStore";
import {
  appendTerminalContextsToPrompt,
  formatTerminalContextLabel,
  type TerminalContextDraft,
  type TerminalContextSelection,
} from "../lib/terminalContext";
import {
  appendElementContextsToPrompt,
  type ElementContextDraft,
  formatElementContextLabel,
} from "../lib/elementContext";
import { appendPreviewAnnotationPrompt } from "../lib/previewAnnotation";
import { appendReviewCommentsToPrompt, type ReviewCommentContext } from "../reviewCommentContext";
import { environmentCatalog } from "../connection/catalog";
import { readCurrentEnvironmentPresentationPolicy } from "../connection/currentEnvironmentPresentation";
import { environmentConnectionActions } from "../connection/environmentPresentationPolicy";
import { useKnownTerminalSessions, useThreadRunningTerminalIds } from "../state/terminalSessions";
import { projectEnvironment } from "../state/projects";
import { useEnvironmentQuery } from "../state/query";
import {
  primaryServerAvailableEditorsAtom,
  primaryServerKeybindingsAtom,
  serverEnvironment,
} from "../state/server";
import { terminalEnvironment } from "../state/terminal";
import { threadEnvironment } from "../state/threads";
import { vcsEnvironment } from "../state/vcs";
import { useEnvironments, usePrimaryEnvironment } from "../state/environments";
import { useProject, useThread, useThreadProposedPlans } from "../state/entities";
import { environmentShell } from "../state/shell";
import { environmentActivity } from "../state/activity";
import { ChatComposer, type ChatComposerHandle } from "./chat/ChatComposer";
import { ExpandedImageDialog } from "./chat/ExpandedImageDialog";
import { PullRequestThreadDialog } from "./PullRequestThreadDialog";
import { MessagesTimeline } from "./chat/MessagesTimeline";
import { ChatHeaderActions } from "./chat/ChatHeaderActions";
import { type ProviderTerminalAction } from "./chat/providerTerminalActions";
import { PanelLayoutControls, RightPanelMaximizeControl } from "./chat/PanelLayoutControls";
import { type ExpandedImagePreview } from "./chat/ExpandedImagePreview";
import { ChatPanelOpeningState, NoActiveThreadState } from "./NoActiveThreadState";
import { resolveEffectiveEnvMode } from "./BranchToolbar.logic";
import { ProviderStatusBanner } from "./chat/ProviderStatusBanner";
import { ThreadErrorBanner } from "./chat/ThreadErrorBanner";
import { ComposerBannerStack, type ComposerBannerStackItem } from "./chat/ComposerBannerStack";
import {
  buildExpiredTerminalContextToastCopy,
  buildLocalDraftThread,
  buildRevertCheckpointConfirmation,
  buildThreadTurnInterruptInput,
  collectUserMessageBlobPreviewUrls,
  createLocalDispatchSnapshot,
  deriveComposerSendState,
  findActiveDeliveryMessage,
  findLastCancellableDeliveryMessage,
  hasServerAcknowledgedLocalDispatch,
  getStartedThreadModelChangeBlockReason,
  LAST_INVOKED_SCRIPT_BY_PROJECT_KEY,
  LastInvokedScriptByProjectSchema,
  type KeyedActivityPage,
  type LocalDispatchSnapshot,
  PullRequestDialogState,
  cloneComposerAttachmentForRetry,
  reconcileBoundedActivityPages,
  resolveActivityPageLoadAction,
  resolveCenterPanelLaunchContext,
  isAgentActivityScopeEnabled,
  resolveActivityScope,
  resolveSendEnvMode,
  revokeBlobPreviewUrl,
  revokeUserMessagePreviewUrls,
  threadErrorAttribution,
  waitForStartedServerThread,
} from "./ChatView.logic";
import { useLocalStorage } from "~/hooks/useLocalStorage";
import { useComposerHandleContext } from "../composerHandleContext";
import { sanitizeThreadErrorMessage } from "~/rpc/transportError";
import { RightPanelSheet } from "./RightPanelSheet";
import { ActivityDock } from "./activity/ActivityDock";
import { activitySnapshotForState } from "./activity/activityPresentation";
import {
  ActivityPanel,
  type ActivityDetailPageData,
  type ActivityDetailQueryResult,
  type ActivityQueryResult,
  type ActivityRosterPageData,
} from "./activity/ActivityPanel";
import { previewEnvironment } from "../state/preview";
import { useAtomCommand } from "../state/use-atom-command";
import { Button } from "./ui/button";
import {
  buildVersionMismatchDismissalKey,
  dismissVersionMismatch,
  isVersionMismatchDismissed,
  resolveServerConfigVersionMismatch,
} from "../versionSkew";
import { useAssetUrls } from "../assets/assetUrls";
import type { FileCommentAnnotationGroup } from "./files/fileCommentAnnotations";
import type { FileEditingSession } from "./files/fileEditingSession";
import { FileEditingSessionRegistry } from "./files/fileEditingSessionRegistry";
import { ProjectFilesPreloader } from "./files/ProjectFilesPreloader";

const ATTACHMENT_ONLY_BOOTSTRAP_PROMPT =
  "[User attached one or more files without additional text. Respond using the conversation context and the attachments.]";
const EMPTY_ACTIVITIES: OrchestrationThreadActivity[] = [];
const EMPTY_PROVIDERS: ServerProvider[] = [];
const EMPTY_PROVIDER_SKILLS: ServerProvider["skills"] = [];
const EMPTY_PENDING_USER_INPUT_ANSWERS: Record<string, PendingUserInputDraftAnswer> = {};
const PreviewPanel = lazy(() =>
  import("./preview/PreviewPanel").then((module) => ({ default: module.PreviewPanel })),
);
const DiffPanel = lazy(() => import("./DiffPanel"));
const SourceControlPanel = lazy(() => import("./SourceControlPanel"));
const FilePreviewPanel = lazy(() => import("./files/FilePreviewPanel"));
const DevicePanel = lazy(() =>
  import("./device/DevicePanel").then((module) => ({ default: module.DevicePanel })),
);
const PullRequestPanelSurface = lazy(() =>
  import("./pullRequests/PullRequestPanelSurface").then((module) => ({
    default: module.PullRequestPanelSurface,
  })),
);
const EMPTY_PENDING_FILE_SURFACE_IDS: ReadonlySet<string> = new Set();
const TYPE_TO_FOCUS_EDITABLE_SELECTOR = [
  "input",
  "textarea",
  "select",
  '[contenteditable="true"]',
  '[contenteditable="plaintext-only"]',
  '[role="textbox"]',
].join(",");
const TYPE_TO_FOCUS_INTERACTIVE_SELECTOR = [
  "button",
  "a[href]",
  "summary",
  '[role="button"]',
  '[role="checkbox"]',
  '[role="menuitem"]',
  '[role="option"]',
  '[role="radio"]',
  '[role="switch"]',
  '[role="tab"]',
].join(",");
const TYPE_TO_FOCUS_FLOATING_LAYER_SELECTOR = [
  '[data-slot="dialog"]',
  '[data-slot="menu-popup"]',
  '[data-slot="select-popup"]',
  '[data-slot="popover-popup"]',
  '[data-slot="combobox-popup"]',
  '[data-slot="autocomplete-popup"]',
].join(",");

type ActivityPageData = ActivityRosterPageData | ActivityDetailPageData;

interface BoundedActivityQuery<Page extends ActivityPageData> extends ActivityQueryResult<Page> {
  readonly failure: unknown | null;
  readonly loadMore: () => void;
  /** Explicit user Retry from the newest page. */
  readonly refresh: () => void;
  /** Automatic re-read from the newest page; keeps an exhausted cut-off latched. */
  readonly revalidate: () => void;
}

function useBoundedActivityQuery<Page extends ActivityPageData>(
  queryKey: string | null,
  atomForCursor: (
    cursor: string | null,
  ) => Atom.Atom<AsyncResult.AsyncResult<Page, unknown>> | null,
): BoundedActivityQuery<Page> {
  const [state, setState] = useState<{
    readonly key: string | null;
    readonly cursor: string | null;
    readonly pages: ReadonlyArray<KeyedActivityPage<Page>>;
    /** The query page last folded into `pages` for this key and cursor. */
    readonly reconciled: Page | null;
  }>({ key: queryKey, cursor: null, pages: [], reconciled: null });
  const current =
    state.key === queryKey ? state : { key: queryKey, cursor: null, pages: [], reconciled: null };
  const queryAtom = useMemo(
    () => (queryKey === null ? null : atomForCursor(current.cursor)),
    [atomForCursor, current.cursor, queryKey],
  );
  const query = useEnvironmentQuery<Page, unknown>(queryAtom);
  const { refresh: refreshQuery, revalidate: revalidateQuery } = query;
  const newestRefreshKeyRef = useRef<{ key: string; retry: boolean } | null>(null);

  const data = query.data;
  if (queryKey !== null && data !== null && current.reconciled !== data) {
    setState((previous) => {
      const pages = previous.key === queryKey ? previous.pages : [];
      return {
        key: queryKey,
        cursor: current.cursor,
        pages: reconcileBoundedActivityPages<Page>(pages, current.cursor, data),
        reconciled: data,
      };
    });
  }

  useEffect(() => {
    const pending = newestRefreshKeyRef.current;
    if (queryKey === null || current.cursor !== null || pending?.key !== queryKey) return;
    newestRefreshKeyRef.current = null;
    if (pending.retry) refreshQuery();
    else revalidateQuery();
  }, [current.cursor, queryKey, refreshQuery, revalidateQuery]);

  const pages = useMemo(() => current.pages.map((entry) => entry.page), [current.pages]);
  const failure = Option.getOrNull(AsyncResult.error(query.emission));
  const loadMore = useCallback(() => {
    const nextCursor = pages.at(-1)?.nextCursor ?? null;
    if (
      resolveActivityPageLoadAction({
        currentCursor: current.cursor,
        nextCursor,
        failed: failure !== null,
      }) === "refresh"
    ) {
      refreshQuery();
      return;
    }
    setState((previous) => ({
      key: queryKey,
      cursor: nextCursor,
      pages: previous.key === queryKey ? previous.pages : [],
      reconciled: null,
    }));
  }, [current.cursor, failure, pages, queryKey, refreshQuery]);
  // Returning to the newest page keeps whether the initiating action was an
  // explicit Retry or an automatic re-read.
  const refreshNewest = useCallback(
    (retry: boolean) => {
      if (queryKey === null) return;
      if (current.cursor === null) {
        if (retry) refreshQuery();
        else revalidateQuery();
        return;
      }
      newestRefreshKeyRef.current = { key: queryKey, retry };
      setState((previous) => ({
        key: queryKey,
        cursor: null,
        pages: previous.key === queryKey ? previous.pages : [],
        reconciled: null,
      }));
    },
    [current.cursor, queryKey, refreshQuery, revalidateQuery],
  );
  const refresh = useCallback(() => refreshNewest(true), [refreshNewest]);
  const revalidate = useCallback(() => refreshNewest(false), [refreshNewest]);

  return useMemo(
    () => ({
      pages,
      loading: query.isPending,
      error: query.error,
      failure,
      loadMore,
      refresh,
      revalidate,
    }),
    [failure, loadMore, pages, query.error, query.isPending, refresh, revalidate],
  );
}

interface ActivityStateTarget {
  readonly environmentId: EnvironmentId;
  readonly input: ActivityScopeRef;
}

const ActivityDockBinding = memo(function ActivityDockBinding({
  target,
  threadRef,
  projectId,
  compact,
  avoidRightPanelSheet,
}: {
  readonly target: ActivityStateTarget;
  readonly threadRef: ScopedThreadRef;
  readonly projectId: ProjectId;
  readonly compact: boolean;
  readonly avoidRightPanelSheet: boolean;
}) {
  const stateValueAtom = useMemo(() => environmentActivity.stateValueAtom(target), [target]);
  const activityState =
    useAtomValue(stateValueAtom) ?? (EMPTY_ENVIRONMENT_ACTIVITY_STATE as EnvironmentActivityState);
  const snapshot = useMemo(() => activitySnapshotForState(activityState), [activityState]);
  const projectKey = useMemo(
    () => scopedProjectKey(scopeProjectRef(target.environmentId, projectId)),
    [projectId, target.environmentId],
  );
  const expanded = useActivityDockStore((state) =>
    selectActivityDockExpanded(state.expandedByProject, projectKey),
  );
  const setExpanded = useActivityDockStore((state) => state.setExpanded);
  const onExpandedChange = useCallback(
    (nextExpanded: boolean) => setExpanded(projectKey, nextExpanded),
    [projectKey, setExpanded],
  );
  const onOpenSection = useCallback(
    (section: ActivitySection) =>
      useRightPanelStore.getState().openActivity(threadRef, section, target.input),
    [target.input, threadRef],
  );

  if (snapshot === null) {
    return null;
  }
  return (
    <ActivityDock
      snapshot={snapshot}
      expanded={expanded}
      compact={compact}
      avoidRightPanelSheet={avoidRightPanelSheet}
      onExpandedChange={onExpandedChange}
      onOpenSection={onOpenSection}
    />
  );
});

const isActivityError = Schema.is(ActivityError);
const isUploadClientError = Schema.is(UploadClientError);
const isUploadEnvironmentUnavailableError = Schema.is(EnvironmentRpcUnavailableError);
const isUploadError = Schema.is(UploadError);
const isAdmissionRpcClientError = Schema.is(RpcClientError.RpcClientError);
const isAdmissionResponseTooLarge = Schema.is(RpcResponseTooLargeError);
function admissionOutcomeIsAmbiguous(cause: Cause.Cause<unknown>): boolean {
  return cause.reasons.some(
    (reason) =>
      Cause.isInterruptReason(reason) ||
      Cause.isDieReason(reason) ||
      (Cause.isFailReason(reason) &&
        (isAdmissionRpcClientError(reason.error) || isAdmissionResponseTooLarge(reason.error))),
  );
}

function isRemovedActivityFailure(failure: unknown): failure is ActivityError {
  return isActivityError(failure) && failure.reason === "notFound";
}

function activityCancellationFailureMessage(failure: unknown): string {
  if (!isActivityError(failure)) {
    return "Unable to stop agents. Try again.";
  }
  switch (failure.reason) {
    case "cancellationUnsupported":
      return "Stopping this agent is not supported.";
    case "staleScope":
    case "staleActor":
    case "staleOperation":
      return "Activity changed before the request could be applied. Refresh and try again.";
    case "providerUnavailable":
      return "The provider is unavailable. Try again when it reconnects.";
    case "targetUnavailable":
      return "This agent can no longer be stopped.";
    case "partialCancellation":
      return "Some agents are still running.";
    case "dispatchTimeout":
      return "Stopping agents timed out. Some agents may still be running.";
    default:
      return "Unable to stop agents. Try again.";
  }
}

const ActivityPanelBinding = memo(function ActivityPanelBinding({
  target,
  threadRef,
  surface,
  timestampFormat,
}: {
  readonly target: ActivityStateTarget;
  readonly threadRef: ScopedThreadRef;
  readonly surface: ActivityRightPanelSurface;
  readonly timestampFormat: TimestampFormat;
}) {
  const stateValueAtom = useMemo(() => environmentActivity.stateValueAtom(target), [target]);
  const stateSourceAtom = useMemo(() => environmentActivity.stateAtom(target), [target]);
  const activityState =
    useAtomValue(stateValueAtom) ?? (EMPTY_ENVIRONMENT_ACTIVITY_STATE as EnvironmentActivityState);
  const refreshSnapshot = useAtomRefresh(stateSourceAtom);
  const cancelSubtree = useAtomCommand(environmentActivity.cancelSubtree, {
    reportFailure: false,
  });
  const retrySubtreeCancellation = useAtomCommand(environmentActivity.retrySubtreeCancellation, {
    reportFailure: false,
  });
  const [cancellationFailure, setCancellationFailure] = useState<{
    readonly message: string;
    readonly targetKey: string;
    readonly controlRevision: number;
  } | null>(null);
  const snapshot = useMemo(() => activitySnapshotForState(activityState), [activityState]);
  const section = surface.section;
  const queryBaseKey =
    snapshot === null ? null : `${threadRef.environmentId}:${snapshot.scopeId}:${section}`;
  const activeRosterAtomForCursor = useCallback(
    (cursor: string | null) =>
      snapshot === null
        ? null
        : (environmentActivity.roster({
            environmentId: threadRef.environmentId,
            input: {
              scope: snapshot.scope,
              scopeId: snapshot.scopeId,
              section,
              bucket: "active",
              ...(cursor === null ? {} : { cursor }),
              limit: 200,
            },
          }) as Atom.Atom<AsyncResult.AsyncResult<ActivityRosterPageData, unknown>>),
    [section, snapshot, threadRef.environmentId],
  );
  const doneRosterAtomForCursor = useCallback(
    (cursor: string | null) =>
      snapshot === null
        ? null
        : (environmentActivity.roster({
            environmentId: threadRef.environmentId,
            input: {
              scope: snapshot.scope,
              scopeId: snapshot.scopeId,
              section,
              bucket: "done",
              ...(cursor === null ? {} : { cursor }),
              limit: 200,
            },
          }) as Atom.Atom<AsyncResult.AsyncResult<ActivityRosterPageData, unknown>>),
    [section, snapshot, threadRef.environmentId],
  );
  const activeRoster = useBoundedActivityQuery(
    queryBaseKey === null ? null : `${queryBaseKey}:active`,
    activeRosterAtomForCursor,
  );
  const doneRoster = useBoundedActivityQuery(
    queryBaseKey === null ? null : `${queryBaseKey}:done`,
    doneRosterAtomForCursor,
  );
  const selectedRecordKind = surface.selectedRecordKind;
  const selectedRecordId = surface.selectedRecordId;
  const detailQueryKey =
    queryBaseKey !== null && selectedRecordKind !== null && selectedRecordId !== null
      ? `${queryBaseKey}:${selectedRecordKind}:${selectedRecordId}`
      : null;
  const detailAtomForCursor = useCallback(
    (cursor: string | null) =>
      snapshot === null || selectedRecordKind === null || selectedRecordId === null
        ? null
        : (environmentActivity.detail({
            environmentId: threadRef.environmentId,
            input: {
              scope: snapshot.scope,
              scopeId: snapshot.scopeId,
              recordKind: selectedRecordKind,
              recordId: selectedRecordId as ActivityRecordId,
              ...(cursor === null ? {} : { cursor }),
              limit: 200,
            },
          }) as Atom.Atom<AsyncResult.AsyncResult<ActivityDetailPageData, unknown>>),
    [selectedRecordId, selectedRecordKind, snapshot, threadRef.environmentId],
  );
  const detailQuery = useBoundedActivityQuery(detailQueryKey, detailAtomForCursor);
  const revalidateActiveRoster = activeRoster.revalidate;
  const revalidateDoneRoster = doneRoster.revalidate;
  const revalidateDetail = detailQuery.revalidate;
  const refreshQueriesRef = useRef<() => void>(() => undefined);
  useLayoutEffect(() => {
    refreshQueriesRef.current = () => {
      revalidateActiveRoster();
      revalidateDoneRoster();
      revalidateDetail();
    };
  }, [revalidateActiveRoster, revalidateDetail, revalidateDoneRoster]);
  // Every change to the queried scope, record, or snapshot revision schedules
  // one coalesced revalidation.
  const queryRevalidationKey =
    snapshot === null
      ? null
      : JSON.stringify([queryBaseKey, detailQueryKey, snapshot.revision, snapshot.scopeId]);
  const refreshTimerRef = useRef<ReturnType<typeof globalThis.setTimeout> | null>(null);
  useEffect(() => {
    if (queryRevalidationKey === null) {
      return;
    }
    if (refreshTimerRef.current !== null) {
      return;
    }
    refreshTimerRef.current = globalThis.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshQueriesRef.current();
    }, 100);
  }, [queryRevalidationKey]);
  useEffect(
    () => () => {
      if (refreshTimerRef.current !== null) {
        globalThis.clearTimeout(refreshTimerRef.current);
      }
    },
    [],
  );
  const detail = useMemo<ActivityDetailQueryResult | null>(
    () =>
      selectedRecordKind !== null && selectedRecordId !== null
        ? {
            recordKind: selectedRecordKind as ActivityRecordKind,
            recordId: selectedRecordId as ActivityRecordId,
            pages: detailQuery.pages,
            loading: detailQuery.loading,
            error: detailQuery.error,
            ...(isRemovedActivityFailure(detailQuery.failure) ? { removed: true } : {}),
          }
        : null,
    [
      detailQuery.error,
      detailQuery.failure,
      detailQuery.loading,
      detailQuery.pages,
      selectedRecordId,
      selectedRecordKind,
    ],
  );
  const roster = useMemo(
    () => ({ active: activeRoster, done: doneRoster }),
    [activeRoster, doneRoster],
  );
  const onNavigate = useCallback(
    (
      route: Pick<ActivityRightPanelSurface, "section" | "selectedRecordKind" | "selectedRecordId">,
    ) => useRightPanelStore.getState().navigateActivity(threadRef, route),
    [threadRef],
  );
  const loadMoreActiveRoster = activeRoster.loadMore;
  const loadMoreDoneRoster = doneRoster.loadMore;
  const onLoadMoreRoster = useCallback(
    (bucket: "active" | "done") =>
      bucket === "active" ? loadMoreActiveRoster() : loadMoreDoneRoster(),
    [loadMoreActiveRoster, loadMoreDoneRoster],
  );
  const cancellationTarget = useMemo(
    () =>
      snapshot !== null &&
      snapshot.scope._tag === "thread" &&
      snapshot.capabilities.targetedActorCancellation
        ? {
            environmentId: threadRef.environmentId,
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            controlRevision: snapshot.control.revision,
            key: JSON.stringify([
              threadRef.environmentId,
              snapshot.scope._tag,
              snapshot.scope.threadId,
              snapshot.scopeId,
            ]),
          }
        : null,
    [snapshot, threadRef.environmentId],
  );
  // The panel has one scoped mutation banner, so the newest cancel or retry invocation owns it.
  const cancellationOwnershipRef = useRef<{
    targetKey: string | null;
    controlRevision: number;
    invocation: number;
  }>({ targetKey: null, controlRevision: -1, invocation: 0 });
  const currentCancellationTargetKey = cancellationTarget?.key ?? null;
  const currentCancellationControlRevision = snapshot?.control.revision ?? -1;
  useLayoutEffect(() => {
    const ownership = cancellationOwnershipRef.current;
    ownership.targetKey = currentCancellationTargetKey;
    ownership.controlRevision = currentCancellationControlRevision;
    ownership.invocation += 1;
    return () => {
      if (
        ownership.targetKey === currentCancellationTargetKey &&
        ownership.controlRevision === currentCancellationControlRevision
      ) {
        ownership.targetKey = null;
        ownership.invocation += 1;
      }
    };
  }, [currentCancellationControlRevision, currentCancellationTargetKey]);
  const cancellationFailureIsCurrent =
    cancellationFailure !== null &&
    cancellationFailure.targetKey === currentCancellationTargetKey &&
    cancellationFailure.controlRevision === currentCancellationControlRevision;
  // A failure belongs to the target and revision it was raised for; drop it as
  // soon as either changes so returning to that target cannot resurface it.
  if (cancellationFailure !== null && !cancellationFailureIsCurrent) {
    setCancellationFailure(null);
  }
  const cancellationError = cancellationFailureIsCurrent ? cancellationFailure.message : null;
  const onCancelActor = useCallback(
    async (actorId: ActivityRecordId, expectedControlRevision: number) => {
      if (cancellationTarget === null) {
        return;
      }
      const ownership = cancellationOwnershipRef.current;
      const invocation = ownership.invocation + 1;
      ownership.invocation = invocation;
      const targetKey = cancellationTarget.key;
      const controlRevision = cancellationTarget.controlRevision;
      setCancellationFailure(null);
      const result = await cancelSubtree({
        environmentId: cancellationTarget.environmentId,
        input: {
          scope: cancellationTarget.scope,
          scopeId: cancellationTarget.scopeId,
          actorId,
          expectedControlRevision,
        },
      });
      if (
        ownership.targetKey !== targetKey ||
        ownership.controlRevision !== controlRevision ||
        ownership.invocation !== invocation
      ) {
        return;
      }
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        setCancellationFailure({
          message: activityCancellationFailureMessage(squashAtomCommandFailure(result)),
          targetKey,
          controlRevision,
        });
      }
    },
    [cancelSubtree, cancellationTarget],
  );
  const onRetryCancellation = useCallback(
    async (rootActorId: ActivityRecordId, expectedOperationRevision: number) => {
      if (cancellationTarget === null) {
        return;
      }
      const ownership = cancellationOwnershipRef.current;
      const invocation = ownership.invocation + 1;
      ownership.invocation = invocation;
      const targetKey = cancellationTarget.key;
      const controlRevision = cancellationTarget.controlRevision;
      setCancellationFailure(null);
      const result = await retrySubtreeCancellation({
        environmentId: cancellationTarget.environmentId,
        input: {
          scope: cancellationTarget.scope,
          scopeId: cancellationTarget.scopeId,
          rootActorId,
          expectedOperationRevision,
        },
      });
      if (
        ownership.targetKey !== targetKey ||
        ownership.controlRevision !== controlRevision ||
        ownership.invocation !== invocation
      ) {
        return;
      }
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        setCancellationFailure({
          message: activityCancellationFailureMessage(squashAtomCommandFailure(result)),
          targetKey,
          controlRevision,
        });
      }
    },
    [cancellationTarget, retrySubtreeCancellation],
  );

  if (snapshot === null) {
    return null;
  }
  return (
    <ActivityPanel
      timestampFormat={timestampFormat}
      route={surface}
      snapshot={snapshot}
      roster={roster}
      detail={detail}
      onNavigate={onNavigate}
      onLoadMoreRoster={onLoadMoreRoster}
      onLoadMoreDetail={detailQuery.loadMore}
      onRefreshSnapshot={refreshSnapshot}
      cancellationError={cancellationError}
      {...(cancellationTarget === null ? {} : { onCancelActor, onRetryCancellation })}
    />
  );
});

type EnvironmentUnavailableState = {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly connection: EnvironmentConnectionPresentation;
};

const WORKSPACE_UNAVAILABLE_REASON =
  "Workspace unavailable. Retry detection or remove it from BiBCode.";

type ThreadPlanCatalogEntry = Pick<Thread, "id" | "proposedPlans">;

export function eventPathContainsSelector(event: Event, selector: string): boolean {
  const path = event.composedPath();
  if (path.length === 0 && event.target) {
    path.push(event.target);
  }
  return path.some((target) => target instanceof Element && target.closest(selector));
}

export function shouldTypeToFocusComposer(event: KeyboardEvent): boolean {
  if (event.defaultPrevented || event.isComposing) return false;
  if (event.metaKey || event.ctrlKey || event.altKey) return false;
  if (event.key.length !== 1) return false;

  if (eventPathContainsSelector(event, TYPE_TO_FOCUS_EDITABLE_SELECTOR)) return false;
  if (eventPathContainsSelector(event, TYPE_TO_FOCUS_INTERACTIVE_SELECTOR)) return false;
  if (document.querySelector(TYPE_TO_FOCUS_FLOATING_LAYER_SELECTOR)) return false;

  return true;
}

function formatOutgoingPrompt(params: {
  provider: ProviderDriverKind;
  model: string | null;
  models: ReadonlyArray<ServerProvider["models"][number]>;
  effort: string | null;
  text: string;
}): string {
  const caps = getProviderModelCapabilities(params.models, params.model, params.provider);
  const promptEffort = resolvePromptInjectedEffort(caps, params.effort);
  return applyClaudePromptEffortPrefix(params.text, promptEffort);
}

type ChatViewRouteProps =
  | {
      environmentId: EnvironmentId;
      threadId: ThreadId;
      onDiffPanelOpen?: () => void;
      reserveTitleBarControlInset?: boolean;
      routeKind: "server";
      draftId?: never;
    }
  | {
      environmentId: EnvironmentId;
      threadId: ThreadId;
      onDiffPanelOpen?: () => void;
      reserveTitleBarControlInset?: boolean;
      routeKind: "draft";
      draftId: DraftId;
    };

// A center "panel" surface renders this same component for a sibling server
// thread (multipanel). The panel thread is ALWAYS a server thread; its identity
// comes from `panelThreadRef`, and singleton chrome/effects are gated off via
// `isPanel` so multiple instances can mount beside the host without corrupting
// route/title/keybinding state.
type ChatViewPanelProps = {
  variant: "panel";
  panelThreadRef: ScopedThreadRef;
  workspaceUnavailable: string | null;
  environmentId?: never;
  threadId?: never;
  routeKind?: never;
  draftId?: never;
  onDiffPanelOpen?: never;
  reserveTitleBarControlInset?: never;
};

type ChatViewProps =
  | (ChatViewRouteProps & { variant?: "host"; panelThreadRef?: never })
  | ChatViewPanelProps;

interface PersistentTerminalLaunchContext {
  cwd: string;
  worktreePath: string | null;
}

function resetRestoredComposerCursor(
  composer: RefObject<ChatComposerHandle | null>,
  prompt: string,
) {
  composer.current?.resetCursorState({ cursor: prompt.length, prompt, detectTrigger: true });
}

// The composer handle can come from context, which React Compiler cannot see is
// a ref; reading it here keeps callbacks keyed on the ref, not its contents.
function readComposerHandle(composer: RefObject<ChatComposerHandle | null>) {
  return composer.current;
}

// Snapshot read of the composer draft store for render-time fallbacks that
// deliberately do not subscribe to it.
function readComposerDraftStore() {
  return useComposerDraftStore.getState();
}

function useLocalDispatchState(input: {
  activeThread: Thread | undefined;
  activeLatestTurn: Thread["latestTurn"] | null;
  phase: SessionPhase;
  activePendingApproval: ApprovalRequestId | null;
  activePendingUserInput: ApprovalRequestId | null;
  localError: string | null | undefined;
}) {
  const [localDispatch, setLocalDispatch] = useState<LocalDispatchSnapshot | null>(null);

  const resetLocalDispatch = useCallback(() => {
    setLocalDispatch(null);
  }, []);

  const localDispatchDeliveryState = (() => {
    if (
      !localDispatch ||
      !input.activeThread ||
      localDispatch.threadId !== input.activeThread.id ||
      !localDispatch.messageId
    ) {
      return null;
    }
    const messageId = localDispatch.messageId;
    return (
      input.activeThread.messages.find((message) => message.id === messageId)?.delivery?.state ??
      null
    );
  })();
  const serverAcknowledgedLocalDispatch = useMemo(
    () =>
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: input.phase,
        latestTurn: input.activeLatestTurn,
        session: input.activeThread?.session ?? null,
        hasPendingApproval: input.activePendingApproval !== null,
        hasPendingUserInput: input.activePendingUserInput !== null,
        threadError: input.localError,
        deliveryState: localDispatchDeliveryState,
      }),
    [
      input.activeLatestTurn,
      input.activePendingApproval,
      input.activePendingUserInput,
      input.activeThread?.session,
      input.phase,
      input.localError,
      localDispatchDeliveryState,
      localDispatch,
    ],
  );
  const activeLocalDispatch = serverAcknowledgedLocalDispatch ? null : localDispatch;
  const cancellableDelivery = input.activeThread
    ? findLastCancellableDeliveryMessage(input.activeThread.messages)
    : null;
  const activeDelivery = input.activeThread
    ? findActiveDeliveryMessage(input.activeThread.messages)
    : null;
  const beginLocalDispatch = useCallback(
    (options?: { preparingWorktree?: boolean; threadId?: ThreadId; messageId?: MessageId }) => {
      const preparingWorktree = Boolean(options?.preparingWorktree);
      setLocalDispatch((current) => {
        const active = serverAcknowledgedLocalDispatch ? null : current;
        if (active) {
          return active.preparingWorktree === preparingWorktree
            ? active
            : { ...active, preparingWorktree };
        }
        return createLocalDispatchSnapshot(input.activeThread, {
          ...options,
          threadError: input.localError ?? null,
        });
      });
    },
    [input.activeThread, input.localError, serverAcknowledgedLocalDispatch],
  );

  return {
    beginLocalDispatch,
    resetLocalDispatch,
    localDispatchStartedAt: activeLocalDispatch?.startedAt ?? null,
    activeDeliveryStartedAt: activeDelivery?.createdAt ?? null,
    cancellableDeliveryThreadId: cancellableDelivery ? (input.activeThread?.id ?? null) : null,
    cancellableDeliveryMessageId: cancellableDelivery?.id ?? null,
    canCancelPendingSend: cancellableDelivery !== null,
    isPreparingWorktree: activeLocalDispatch?.preparingWorktree ?? false,
    isSendBusy: activeLocalDispatch !== null || cancellableDelivery !== null,
    isSendActivelyWorking: activeLocalDispatch !== null || activeDelivery !== null,
  };
}

interface PersistentThreadTerminalPanelProps {
  threadRef: ScopedThreadRef;
  surface: Extract<RightPanelSurface, { kind: "terminal" }>;
  launchContext: PersistentTerminalLaunchContext | null;
  focusRequestId: number;
  keybindings: ResolvedKeybindingsConfig;
  onAddTerminalContext: (selection: TerminalContextSelection) => void;
  onSplitTerminal: () => void;
  onSplitTerminalVertical: () => void;
  onNewTerminal: () => void;
  onActiveTerminalChange: (terminalId: string) => void;
  onCloseTerminal: (terminalId: string) => void;
  splitShortcutLabel?: string | undefined;
  splitVerticalShortcutLabel?: string | undefined;
  newShortcutLabel?: string | undefined;
  closeShortcutLabel?: string | undefined;
  workspaceUnavailable: string | null;
}

const PersistentThreadTerminalPanel = memo(function PersistentThreadTerminalPanel({
  threadRef,
  surface,
  launchContext,
  focusRequestId,
  keybindings,
  onAddTerminalContext,
  onSplitTerminal,
  onSplitTerminalVertical,
  onNewTerminal,
  onActiveTerminalChange,
  onCloseTerminal,
  splitShortcutLabel,
  splitVerticalShortcutLabel,
  newShortcutLabel,
  closeShortcutLabel,
  workspaceUnavailable,
}: PersistentThreadTerminalPanelProps) {
  const serverThread = useThread(threadRef);
  const draftThread = useComposerDraftStore((store) => store.getDraftThreadByRef(threadRef));
  const projectRef = serverThread
    ? scopeProjectRef(serverThread.environmentId, serverThread.projectId)
    : draftThread
      ? scopeProjectRef(draftThread.environmentId, draftThread.projectId)
      : null;
  const project = useProject(projectRef);
  const knownTerminalSessions = useKnownTerminalSessions({
    environmentId: threadRef.environmentId,
    threadId: threadRef.threadId,
  });
  const threadWorktreePath = serverThread?.worktreePath ?? draftThread?.worktreePath ?? null;
  const activeSummary =
    knownTerminalSessions.find((session) => session.target.terminalId === surface.activeTerminalId)
      ?.state.summary ?? null;
  const worktreePath =
    launchContext?.worktreePath ?? activeSummary?.worktreePath ?? threadWorktreePath;
  const cwd = useMemo(
    () =>
      launchContext?.cwd ??
      activeSummary?.cwd ??
      (project
        ? projectScriptCwd({
            project: { cwd: project.workspaceRoot },
            worktreePath,
          })
        : null),
    [activeSummary?.cwd, launchContext?.cwd, project, worktreePath],
  );
  const runtimeEnv = useMemo(
    () =>
      project
        ? projectScriptRuntimeEnv({
            project: { cwd: project.workspaceRoot },
            worktreePath,
          })
        : {},
    [project, worktreePath],
  );
  const terminalLabelsById = useMemo(() => {
    const labels = new Map<string, string>();
    for (const terminalId of surface.terminalIds) {
      const summary =
        knownTerminalSessions.find((session) => session.target.terminalId === terminalId)?.state
          .summary ?? null;
      labels.set(terminalId, resolveTerminalSessionLabel(terminalId, summary));
    }
    return labels;
  }, [knownTerminalSessions, surface.terminalIds]);
  const terminalLaunchLocationsById = useMemo(() => {
    const locations = new Map<
      string,
      {
        readonly cwd: string;
        readonly worktreePath: string | null;
        readonly runtimeEnv: Record<string, string>;
      }
    >();
    for (const terminalId of surface.terminalIds) {
      const summary =
        knownTerminalSessions.find((session) => session.target.terminalId === terminalId)?.state
          .summary ?? null;
      const terminalWorktreePath =
        launchContext?.worktreePath ?? summary?.worktreePath ?? threadWorktreePath;
      const terminalCwd =
        launchContext?.cwd ??
        summary?.cwd ??
        (project
          ? projectScriptCwd({
              project: { cwd: project.workspaceRoot },
              worktreePath: terminalWorktreePath,
            })
          : null);
      if (!terminalCwd || !project) continue;
      locations.set(terminalId, {
        cwd: terminalCwd,
        worktreePath: terminalWorktreePath,
        runtimeEnv: projectScriptRuntimeEnv({
          project: { cwd: project.workspaceRoot },
          worktreePath: terminalWorktreePath,
        }),
      });
    }
    return locations;
  }, [
    knownTerminalSessions,
    launchContext?.cwd,
    launchContext?.worktreePath,
    project,
    surface.terminalIds,
    threadWorktreePath,
  ]);

  if (!project || !cwd) return null;

  return (
    <ThreadTerminalPanel
      owner="right-panel"
      threadRef={threadRef}
      threadId={threadRef.threadId}
      projectId={project.id}
      cwd={cwd}
      worktreePath={worktreePath}
      runtimeEnv={runtimeEnv}
      terminalIds={surface.terminalIds}
      activeTerminalId={surface.activeTerminalId}
      terminalGroups={[
        {
          id: surface.id,
          terminalIds: surface.terminalIds,
          ...(surface.splitDirection === "vertical" ? { splitDirection: "vertical" as const } : {}),
        },
      ]}
      activeTerminalGroupId={surface.id}
      focusRequestId={focusRequestId}
      onSplitTerminal={onSplitTerminal}
      onSplitTerminalVertical={onSplitTerminalVertical}
      onNewTerminal={onNewTerminal}
      splitShortcutLabel={splitShortcutLabel}
      splitVerticalShortcutLabel={splitVerticalShortcutLabel}
      newShortcutLabel={newShortcutLabel}
      closeShortcutLabel={closeShortcutLabel}
      workspaceUnavailable={workspaceUnavailable}
      onActiveTerminalChange={onActiveTerminalChange}
      onCloseTerminal={onCloseTerminal}
      onAddTerminalContext={onAddTerminalContext}
      terminalLabelsById={terminalLabelsById}
      terminalLaunchLocationsById={terminalLaunchLocationsById}
      keybindings={keybindings}
    />
  );
});

interface LiveCenterPanelWorkspaceProps {
  readonly workspaceRef: RefObject<CenterPanelWorkspaceHandle | null>;
  readonly state: ThreadCenterPanelState;
  readonly hostLabel: string;
  readonly terminalLabelsById: ReadonlyMap<string, string>;
  readonly renderFocusedActions: CenterPanelWorkspaceProps["renderFocusedActions"];
  readonly hostChatSurfaceBody: ReactNode;
  readonly hostThread: Thread;
  readonly hostThreadRef: ScopedThreadRef;
  readonly centerTerminalLaunchContext: {
    readonly cwd: string;
    readonly worktreePath: string | null;
    readonly runtimeEnv: Record<string, string>;
  } | null;
  readonly keybindings: ResolvedKeybindingsConfig;
  readonly terminalFocusRequestId: number;
  readonly onAddTerminalContext: (selection: TerminalContextSelection) => void;
  readonly onFocusGroup: CenterPanelWorkspaceProps["onFocusGroup"];
  readonly onActivate: CenterPanelWorkspaceProps["onActivate"];
  readonly onCloseSurface: CenterPanelWorkspaceProps["onCloseSurface"];
  readonly onCloseOtherSurfaces: CenterPanelWorkspaceProps["onCloseOtherSurfaces"];
  readonly onCloseSurfacesToRight: CenterPanelWorkspaceProps["onCloseSurfacesToRight"];
  readonly onCloseAllSurfaces: CenterPanelWorkspaceProps["onCloseAllSurfaces"];
  readonly onDropSurface: CenterPanelWorkspaceProps["onDropSurface"];
  readonly onMergeGroup: CenterPanelWorkspaceProps["onMergeGroup"];
  readonly onSetSplitRatio: CenterPanelWorkspaceProps["onSetSplitRatio"];
  readonly workspaceUnavailable: string | null;
}

const LiveCenterPanelWorkspace = memo(function LiveCenterPanelWorkspace({
  workspaceRef,
  state,
  hostLabel,
  terminalLabelsById,
  renderFocusedActions,
  hostChatSurfaceBody,
  hostThread,
  hostThreadRef,
  centerTerminalLaunchContext,
  keybindings,
  terminalFocusRequestId,
  onAddTerminalContext,
  onFocusGroup,
  onActivate,
  onCloseSurface,
  onCloseOtherSurfaces,
  onCloseSurfacesToRight,
  onCloseAllSurfaces,
  onDropSurface,
  onMergeGroup,
  onSetSplitRatio,
  workspaceUnavailable,
}: LiveCenterPanelWorkspaceProps) {
  const renderCenterSurface = useCallback(
    (surface: CenterSurface, context: CenterPanelSurfaceRenderContext) => {
      switch (surface.kind) {
        case "chat-host":
          return hostChatSurfaceBody;
        case "chat":
          return (
            <ChatView
              variant="panel"
              panelThreadRef={scopeThreadRef(hostThreadRef.environmentId, surface.threadId)}
              workspaceUnavailable={workspaceUnavailable}
            />
          );
        case "terminal":
          return (
            <CenterTerminalPanel
              threadRef={hostThreadRef}
              projectId={hostThread.projectId}
              surface={surface}
              launchContext={centerTerminalLaunchContext}
              keybindings={keybindings}
              focusRequestId={terminalFocusRequestId}
              focusEligible={context.focused}
              onAddTerminalContext={onAddTerminalContext}
              onClose={() => onCloseSurface(context.groupId, surface)}
              workspaceUnavailable={workspaceUnavailable}
            />
          );
      }
    },
    [
      centerTerminalLaunchContext,
      hostChatSurfaceBody,
      hostThread.projectId,
      hostThreadRef,
      keybindings,
      onAddTerminalContext,
      onCloseSurface,
      terminalFocusRequestId,
      workspaceUnavailable,
    ],
  );

  return (
    <CenterPanelWorkspace
      ref={workspaceRef}
      state={state}
      hostLabel={hostLabel}
      terminalLabelsById={terminalLabelsById}
      renderFocusedActions={renderFocusedActions}
      renderSurface={renderCenterSurface}
      onFocusGroup={onFocusGroup}
      onActivate={onActivate}
      onCloseSurface={onCloseSurface}
      onCloseOtherSurfaces={onCloseOtherSurfaces}
      onCloseSurfacesToRight={onCloseSurfacesToRight}
      onCloseAllSurfaces={onCloseAllSurfaces}
      onDropSurface={onDropSurface}
      onMergeGroup={onMergeGroup}
      onSetSplitRatio={onSetSplitRatio}
    />
  );
});

function ChatViewContent(props: ChatViewProps) {
  // In "panel" variant, thread identity comes from panelThreadRef (always a
  // server thread) instead of the route. Resolving environmentId/threadId here
  // makes routeThreadRef and every downstream derivation target the panel
  // thread automatically. `isPanel` additionally gates singleton chrome/effects.
  const isPanel = props.variant === "panel";
  const centerPanelWorkspaceRef = useRef<CenterPanelWorkspaceHandle | null>(null);
  const centerTerminalRouteBindingRef = useRef<{
    readonly threadKey: string | null;
    readonly revision: number;
  }>({ threadKey: null, revision: 0 });
  const environmentId =
    props.variant === "panel" ? props.panelThreadRef.environmentId : props.environmentId;
  const threadId = props.variant === "panel" ? props.panelThreadRef.threadId : props.threadId;
  const routeKind: "server" | "draft" = props.variant === "panel" ? "server" : props.routeKind;
  const onDiffPanelOpen = props.variant === "panel" ? undefined : props.onDiffPanelOpen;
  const draftId = props.variant !== "panel" && props.routeKind === "draft" ? props.draftId : null;
  const routeThreadRef = useMemo(
    () => scopeThreadRef(environmentId, threadId),
    [environmentId, threadId],
  );
  const routeThreadKey = useMemo(() => scopedThreadKey(routeThreadRef), [routeThreadRef]);
  const updateProject = useAtomCommand(projectEnvironment.update, { reportFailure: false });
  const upsertKeybinding = useAtomCommand(serverEnvironment.upsertKeybinding, {
    reportFailure: false,
  });
  const openTerminal = useAtomCommand(terminalEnvironment.open, "terminal open");
  const writeTerminal = useAtomCommand(terminalEnvironment.write, "terminal write");
  const closeTerminalMutation = useAtomCommand(terminalEnvironment.close, "terminal close");
  const createPanelThread = useAtomCommand(worktreeEnvironment.createPanel, {
    reportFailure: false,
  });
  const createManagedWorktree = useAtomCommand(worktreeEnvironment.createManaged, {
    reportFailure: false,
  });
  const deleteThread = useAtomCommand(threadEnvironment.delete, { reportFailure: false });
  const updateThreadMetadata = useAtomCommand(threadEnvironment.updateMetadata, {
    reportFailure: false,
  });
  const commitComposerModelSelection = useCallback(
    async (selection: ModelSelection) => {
      if (routeKind !== "server") return;
      const result = await updateThreadMetadata({
        environmentId,
        input: { threadId, modelSelection: selection },
      });
      if (result._tag === "Failure") {
        throw squashAtomCommandFailure(result);
      }
    },
    [environmentId, routeKind, threadId, updateThreadMetadata],
  );
  const setThreadRuntimeMode = useAtomCommand(threadEnvironment.setRuntimeMode, {
    reportFailure: false,
  });
  const setThreadInteractionMode = useAtomCommand(threadEnvironment.setInteractionMode, {
    reportFailure: false,
  });
  const startThreadTurn = useAtomCommand(threadEnvironment.startTurn, { reportFailure: false });
  const stageAttachments = useAtomCommand(threadEnvironment.stageAttachments, {
    reportFailure: false,
  });
  const readAttachmentAdmissionAuthority = useAtomCommand(
    threadEnvironment.attachmentAdmissionAuthority,
    { reportFailure: false },
  );
  const releaseStagedAttachments = useAtomCommand(threadEnvironment.releaseStagedAttachments, {
    reportFailure: false,
  });
  const keepStagedAttachmentsAlive = useAtomCommand(threadEnvironment.keepStagedAttachmentsAlive, {
    reportFailure: false,
    reportDefect: false,
  });
  const steerThreadTurn = useAtomCommand(threadEnvironment.steerTurn, { reportFailure: false });
  const promoteThreadTurn = useAtomCommand(threadEnvironment.promoteTurn, { reportFailure: false });
  const interruptThreadTurn = useAtomCommand(threadEnvironment.interruptTurn, {
    reportFailure: false,
  });
  const resolveTurnDelivery = useAtomCommand(threadEnvironment.resolveDelivery, {
    reportFailure: false,
  });
  const respondToThreadApproval = useAtomCommand(threadEnvironment.respondToApproval, {
    reportFailure: false,
  });
  const respondToThreadUserInput = useAtomCommand(threadEnvironment.respondToUserInput, {
    reportFailure: false,
  });
  const revertThreadCheckpoint = useAtomCommand(threadEnvironment.revertCheckpoint, {
    reportFailure: false,
  });
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: false });
  const closePreview = useAtomCommand(previewEnvironment.close, "preview close");
  const { environments } = useEnvironments();
  const presentation = useMemo(() => readCurrentEnvironmentPresentationPolicy(), []);
  const primaryEnvironment = usePrimaryEnvironment();
  const retryEnvironment = useAtomCommand(environmentCatalog.retryNow, { reportFailure: false });
  const environmentById = useMemo(
    () => new Map(environments.map((environment) => [environment.environmentId, environment])),
    [environments],
  );
  const composerDraftTarget: ScopedThreadRef | DraftId =
    props.variant === "panel" || props.routeKind === "server" ? routeThreadRef : props.draftId;
  const serverThread = useThread(routeKind === "server" ? routeThreadRef : null);
  const markThreadVisited = useUiStateStore((store) => store.markThreadVisited);
  const activeThreadLastVisitedAt = useUiStateStore((store) =>
    routeKind === "server" ? store.threadLastVisitedAtById[routeThreadKey] : undefined,
  );
  const settings = useEnvironmentSettings(environmentId);
  const enableChatAgentActivity = settings.enableChatAgentActivity;
  const enableTerminalAgentActivity = settings.enableTerminalAgentActivity;
  const setStickyComposerModelSelection = useComposerDraftStore(
    (store) => store.setStickyModelSelection,
  );
  const timestampFormat = settings.timestampFormat;
  const autoOpenPlanSidebar = settings.autoOpenPlanSidebar;
  const navigate = useNavigate();
  const { resolvedTheme } = useTheme();
  // Granular store selectors — avoid subscribing to prompt changes.
  const composerRuntimeMode = useComposerDraftStore(
    (store) => store.getComposerDraft(composerDraftTarget)?.runtimeMode ?? null,
  );
  const composerInteractionMode = useComposerDraftStore(
    (store) => store.getComposerDraft(composerDraftTarget)?.interactionMode ?? null,
  );
  const composerActiveProvider = useComposerDraftStore(
    (store) => store.getComposerDraft(composerDraftTarget)?.activeProvider ?? null,
  );
  const setComposerDraftModelSelection = useComposerDraftStore((store) => store.setModelSelection);
  const setComposerDraftRuntimeMode = useComposerDraftStore((store) => store.setRuntimeMode);
  const setComposerDraftInteractionMode = useComposerDraftStore(
    (store) => store.setInteractionMode,
  );
  const clearComposerDraftContent = useComposerDraftStore((store) => store.clearComposerContent);
  const discardComposerDraftContent = useComposerDraftStore(
    (store) => store.discardComposerContent,
  );
  const setDraftThreadContext = useComposerDraftStore((store) => store.setDraftThreadContext);
  const getDraftSessionByLogicalProjectKey = useComposerDraftStore(
    (store) => store.getDraftSessionByLogicalProjectKey,
  );
  const getDraftSession = useComposerDraftStore((store) => store.getDraftSession);
  const setLogicalProjectDraftThreadId = useComposerDraftStore(
    (store) => store.setLogicalProjectDraftThreadId,
  );
  const draftThread = useComposerDraftStore((store) =>
    routeKind === "server"
      ? store.getDraftSessionByRef(routeThreadRef)
      : draftId
        ? store.getDraftSession(draftId)
        : null,
  );
  const promptRef = useRef("");
  const composerAttachmentsRef = useRef<ComposerAttachment[]>([]);
  const composerTerminalContextsRef = useRef<TerminalContextDraft[]>([]);
  const composerElementContextsRef = useRef<ElementContextDraft[]>([]);
  const localComposerRef = useRef<ChatComposerHandle | null>(null);
  const sharedComposerHandle = useComposerHandleContext();
  // The shared composer handle (command palette + global keybindings) is
  // app-level and host-owned. A panel must use its OWN local handle so mounting
  // it beside the host does not clobber the ref that host shortcuts drive.
  const composerRef = isPanel ? localComposerRef : (sharedComposerHandle ?? localComposerRef);
  // The thread the scroll-to-bottom pill was shown on: it shows only there, so a thread switch
  // hides it without a reset, and a show scheduled before the switch lands on the old thread.
  const [scrollPillThreadId, setScrollPillThreadId] = useState<ThreadId | null>(null);
  const [expandedImage, setExpandedImage] = useState<ExpandedImagePreview | null>(null);
  const [optimisticUserMessages, setOptimisticUserMessages] = useState<ChatMessage[]>([]);
  const [attachmentUploads, setAttachmentUploads] = useState<
    Record<string, PendingAttachmentUpload & { readonly threadKey: string }>
  >({});
  const activeAttachmentUploads = useMemo(
    () =>
      Object.fromEntries(
        Object.entries(attachmentUploads).filter(
          ([, progress]) => progress.threadKey === routeThreadKey,
        ),
      ),
    [attachmentUploads, routeThreadKey],
  );
  const hasActiveAttachmentUpload = Object.keys(activeAttachmentUploads).length > 0;
  const attachmentSendControllers = useRef(
    new Map<
      MessageId,
      {
        readonly threadKey: string;
        readonly controller: AbortController;
        readonly restore: () => void;
        phase: "staging" | "admitting";
      }
    >(),
  );
  const stagedAttachmentAttempts = attachmentAdmissionOwner;
  const applicationSendBusy = useSyncExternalStore(
    useCallback(
      (listener) => attachmentAdmissionOwner.subscribe(routeThreadRef, listener),
      [routeThreadRef],
    ),
    useCallback(() => attachmentAdmissionOwner.isBusy(routeThreadRef), [routeThreadRef]),
    () => false,
  );
  const onCancelAttachmentUpload = useCallback(
    (messageId: MessageId) => {
      const attempt = attachmentSendControllers.current.get(messageId);
      if (
        !attempt ||
        attempt.phase !== "staging" ||
        attempt.threadKey !== routeThreadKey ||
        attempt.controller.signal.aborted
      )
        return;
      attempt.controller.abort();
      attempt.restore();
    },
    [routeThreadKey],
  );
  const optimisticUserMessagesRef = useRef(optimisticUserMessages);
  useLayoutEffect(() => {
    optimisticUserMessagesRef.current = optimisticUserMessages;
  }, [optimisticUserMessages]);
  const [localDraftErrorsByDraftId, setLocalDraftErrorsByDraftId] = useState<
    Record<string, string | null>
  >({});
  const [localServerErrorsByThreadKey, setLocalServerErrorsByThreadKey] = useState<
    Record<string, string | null>
  >({});
  const [isConnecting, _setIsConnecting] = useState(false);
  const [isRevertingCheckpoint, setIsRevertingCheckpoint] = useState(false);
  const [maximizedRightPanelThreadKey, setMaximizedRightPanelThreadKey] = useState<string | null>(
    null,
  );
  const [respondingRequestIds, setRespondingRequestIds] = useState<ApprovalRequestId[]>([]);
  const [respondingUserInputRequestIds, setRespondingUserInputRequestIds] = useState<
    ApprovalRequestId[]
  >([]);
  const [pendingUserInputAnswersByRequestId, setPendingUserInputAnswersByRequestId] = useState<
    Record<string, Record<string, PendingUserInputDraftAnswer>>
  >({});
  const [pendingUserInputQuestionIndexByRequestId, setPendingUserInputQuestionIndexByRequestId] =
    useState<Record<string, number>>({});
  const shouldUsePlanSidebarSheet = useMediaQuery(RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY);
  const shouldUseCompactActivityDock = useMediaQuery(ACTIVITY_DOCK_COMPACT_MEDIA_QUERY);
  // Tracks whether the user explicitly dismissed the sidebar for the active turn.
  const planSidebarDismissedForTurnRef = useRef<string | null>(null);
  // When set, the thread-change reset effect will open the sidebar instead of closing it.
  // Used by "Implement in a new thread" to carry the sidebar-open intent across navigation.
  const planSidebarOpenOnNextThreadRef = useRef(false);
  const [terminalFocusRequestId, setTerminalFocusRequestId] = useState(0);
  const [pullRequestDialogState, setPullRequestDialogState] =
    useState<PullRequestDialogState | null>(null);
  const [attachmentPreviewHandoffByMessageId, setAttachmentPreviewHandoffByMessageId] = useState<
    Record<string, string[]>
  >({});
  const [pendingServerThreadEnvMode] = useState<DraftThreadEnvMode | null>(null);
  const [pendingServerThreadBranch] = useState<string | null>();
  const [pendingServerThreadStartFromOriginByThreadId] = useState<Record<string, boolean>>({});
  const [lastInvokedScriptByProjectId, setLastInvokedScriptByProjectId] = useLocalStorage(
    LAST_INVOKED_SCRIPT_BY_PROJECT_KEY,
    {},
    LastInvokedScriptByProjectSchema,
  );
  const legendListRef = useRef<LegendListRef | null>(null);
  const [composerOverlayElement, setComposerOverlayElement] = useState<HTMLDivElement | null>(null);
  const [composerOverlayHeight, setComposerOverlayHeight] = useState(0);
  const isAtEndRef = useRef(true);
  const attachmentPreviewHandoffByMessageIdRef = useRef<Record<string, string[]>>({});
  const attachmentPreviewPromotionInFlightByMessageIdRef = useRef<Record<string, true>>({});
  const sendInFlightRef = useRef(false);

  useLayoutEffect(() => {
    if (!composerOverlayElement) return;

    const updateHeight = () => {
      const nextHeight = Math.ceil(composerOverlayElement.getBoundingClientRect().height);
      if (nextHeight <= 0) return;
      setComposerOverlayHeight((currentHeight) =>
        currentHeight === nextHeight ? currentHeight : nextHeight,
      );
    };

    updateHeight();
    if (typeof ResizeObserver === "undefined") return;

    const observer = new ResizeObserver(updateHeight);
    observer.observe(composerOverlayElement);
    return () => observer.disconnect();
  }, [composerOverlayElement]);

  const fallbackDraftProjectRef = draftThread
    ? scopeProjectRef(draftThread.environmentId, draftThread.projectId)
    : null;
  const fallbackDraftProject = useProject(fallbackDraftProjectRef);
  const fallbackDraftResolution = useMemo(() => {
    if (!draftThread) return null;
    const targetInstanceId =
      fallbackDraftProject?.defaultModelSelection?.instanceId ??
      readComposerDraftStore().stickyActiveProvider ??
      defaultInstanceIdForDriver(ProviderDriverKind.make("codex"));
    const providers = environmentById.get(draftThread.environmentId)?.serverConfig?.providers ?? [];
    return resolveProviderSessionSelectionForInstance({
      instanceId: targetInstanceId,
      providers,
      settings,
      projectSelection: fallbackDraftProject?.defaultModelSelection ?? null,
    });
  }, [draftThread, environmentById, fallbackDraftProject?.defaultModelSelection, settings]);
  const fallbackDraftModelSelection = fallbackDraftResolution?.modelSelection ?? null;
  const legacyDraftMissingStoredModelSelection = useMemo(() => {
    if (!draftThread) return false;
    const composerDraft = readComposerDraftStore().getComposerDraft(composerDraftTarget);
    return Object.keys(composerDraft?.modelSelectionByProvider ?? {}).length === 0;
  }, [composerDraftTarget, draftThread]);
  const warnedLegacyDraftFallbacksRef = useRef(new Set<string>());
  const localDraftError =
    routeKind === "server" && serverThread
      ? null
      : ((draftId ? localDraftErrorsByDraftId[draftId] : null) ?? null);
  const localServerError = localServerErrorsByThreadKey[routeThreadKey] ?? null;
  const localDraftThread = useMemo(
    () =>
      draftThread
        ? buildLocalDraftThread(
            threadId,
            draftThread,
            fallbackDraftModelSelection ?? {
              instanceId: ProviderInstanceId.make("codex"),
              model: DEFAULT_MODEL,
            },
          )
        : undefined,
    [draftThread, fallbackDraftModelSelection, threadId],
  );
  const isServerThread = routeKind === "server" && serverThread !== null;
  const activeThread = isServerThread ? serverThread : localDraftThread;
  const threadError = isServerThread
    ? (localServerError ?? serverThread?.session?.lastError ?? null)
    : localDraftError;
  // Attribute the banner. `session.lastError` is mixed-provenance and
  // `localServerError` is a BiBCode command that failed, so rendering them
  // identically made a provider outage read as a BiBCode defect.
  const threadErrorAttributionText =
    threadError === null || !isServerThread
      ? null
      : threadErrorAttribution({
          isBiBCodeAction: localServerError !== null,
          errorClass: serverThread?.session?.lastErrorClass ?? null,
          providerName: serverThread?.session?.providerName ?? null,
        });
  const runtimeMode = composerRuntimeMode ?? activeThread?.runtimeMode ?? DEFAULT_RUNTIME_MODE;
  const interactionMode =
    composerInteractionMode ?? activeThread?.interactionMode ?? DEFAULT_INTERACTION_MODE;
  const isLocalDraftThread = !isServerThread && localDraftThread !== undefined;
  const activeThreadId = activeThread?.id ?? null;
  const runningTerminalIds = useThreadRunningTerminalIds({
    environmentId: activeThread?.environmentId ?? null,
    threadId: activeThreadId,
  });
  const activeThreadKnownSessionsRaw = useKnownTerminalSessions({
    environmentId: activeThread?.environmentId ?? null,
    threadId: activeThreadId,
  });
  const activeThreadKnownSessions = useMemo(() => {
    if (activeThreadId === null) {
      return [];
    }
    return activeThreadKnownSessionsRaw.filter(
      (session) => session.target.threadId === activeThreadId,
    );
  }, [activeThreadId, activeThreadKnownSessionsRaw]);
  const activeKnownTerminalIds = useMemo(
    () => activeThreadKnownSessions.map((session) => session.target.terminalId),
    [activeThreadKnownSessions],
  );
  const activeTerminalLabelsById = useMemo(() => {
    const labels = new Map<string, string>();
    for (const session of activeThreadKnownSessions) {
      labels.set(
        session.target.terminalId,
        resolveTerminalSessionLabel(session.target.terminalId, session.state.summary),
      );
    }
    return labels;
  }, [activeThreadKnownSessions]);
  const activeThreadIdentityEnvironmentId = activeThread?.environmentId;
  const activeThreadIdentityId = activeThread?.id;
  const activeThreadRef = useMemo(
    () =>
      activeThreadIdentityEnvironmentId === undefined || activeThreadIdentityId === undefined
        ? null
        : scopeThreadRef(activeThreadIdentityEnvironmentId, activeThreadIdentityId),
    [activeThreadIdentityEnvironmentId, activeThreadIdentityId],
  );
  const activeThreadKey = activeThreadRef ? scopedThreadKey(activeThreadRef) : null;
  useLayoutEffect(() => {
    const binding = {
      threadKey: activeThreadKey,
      revision: centerTerminalRouteBindingRef.current.revision + 1,
    };
    centerTerminalRouteBindingRef.current = binding;
    return () => {
      if (centerTerminalRouteBindingRef.current === binding) {
        centerTerminalRouteBindingRef.current = {
          threadKey: null,
          revision: binding.revision + 1,
        };
      }
    };
  }, [activeThreadKey]);
  // Center multipanel state (host variant only; a panel instance never hosts
  // its own sub-panels). The host chat surface is always present at index 0.
  const centerPanelByThreadKey = useCenterPanelStore((state) => state.byThreadKey);
  const retireTerminalResource = useCallback(
    (target: TerminalRetirementTarget) =>
      retireTerminalSession(target, {
        closeSession: closeTerminalMutation,
        writeExit: ({ environmentId, threadId, terminalId, data }) => {
          enqueueTerminalInput({
            environmentId,
            threadId,
            terminalId,
            data,
            fallbackError: "Terminal exit fallback failed",
            write: (nextData) =>
              writeTerminal({
                environmentId,
                input: { threadId, terminalId, data: nextData },
              }),
          });
        },
        releaseInput: ({ environmentId, threadId, terminalId }) => {
          releaseTerminalUiResources(environmentId, threadId, terminalId);
        },
      }),
    [closeTerminalMutation, writeTerminal],
  );
  const closeCenterTerminalResource = useCallback(
    (hostRef: ScopedThreadRef, surface: Extract<CenterSurface, { kind: "terminal" }>) => {
      void retireTerminalResource({
        environmentId: hostRef.environmentId,
        threadId: hostRef.threadId,
        terminalId: surface.terminalId,
      });
    },
    [retireTerminalResource],
  );
  const centerPanelActions = useCenterPanelActions({
    onCloseTerminal: closeCenterTerminalResource,
  });
  const centerPanelState = selectThreadCenterPanelState(
    centerPanelByThreadKey,
    isPanel ? null : activeThreadRef,
  );
  const closeCenterPanelSurface = useCallback(
    (groupId: string, surface: CenterSurface) => {
      if (!activeThreadRef) return;
      centerPanelActions.closeSurface(activeThreadRef, groupId, surface);
    },
    [activeThreadRef, centerPanelActions],
  );
  const closeOtherCenterPanelSurfaces = useCallback(
    (groupId: string, surface: CenterSurface) => {
      if (!activeThreadRef) return;
      centerPanelActions.closeOtherSurfaces(activeThreadRef, groupId, surface);
    },
    [activeThreadRef, centerPanelActions],
  );
  const closeCenterPanelSurfacesToRight = useCallback(
    (groupId: string, surface: CenterSurface) => {
      if (!activeThreadRef) return;
      centerPanelActions.closeSurfacesToRight(activeThreadRef, groupId, surface);
    },
    [activeThreadRef, centerPanelActions],
  );
  const closeAllCenterPanelSurfaces = useCallback(
    (groupId: string) => {
      if (!activeThreadRef) return;
      centerPanelActions.closeAllSurfaces(activeThreadRef, groupId);
    },
    [activeThreadRef, centerPanelActions],
  );
  const focusCenterPanelGroup = useCallback(
    (groupId: string) => {
      if (!activeThreadRef) return;
      useCenterPanelStore.getState().focusGroup(activeThreadRef, groupId);
    },
    [activeThreadRef],
  );
  const activateCenterPanelSurface = useCallback(
    (groupId: string, surface: CenterSurface) => {
      if (!activeThreadRef) return;
      centerPanelActions.activateSurface(activeThreadRef, groupId, surface.id);
    },
    [activeThreadRef, centerPanelActions],
  );
  const dropCenterPanelSurface = useCallback(
    (surfaceId: string, target: CenterPanelDropRequest) => {
      if (!activeThreadRef) return;
      useCenterPanelStore.getState().dropSurface(activeThreadRef, surfaceId, target);
    },
    [activeThreadRef],
  );
  const mergeCenterPanelGroup = useCallback(
    (groupId: string) => {
      if (!activeThreadRef) return;
      useCenterPanelStore.getState().mergeGroup(activeThreadRef, groupId);
    },
    [activeThreadRef],
  );
  const setCenterPanelSplitRatio = useCallback(
    (path: CenterPanelLayoutPath, ratio: number) => {
      if (!activeThreadRef) return;
      useCenterPanelStore.getState().setSplitRatio(activeThreadRef, path, ratio);
    },
    [activeThreadRef],
  );
  const focusedCenterSurface = selectFocusedCenterSurface(centerPanelState);
  const siblingChatOwnsCenter = !isPanel && focusedCenterSurface?.kind === "chat";
  const [timelineAnchor, setTimelineAnchor] = useState<{
    readonly threadKey: string | null;
    readonly messageId: MessageId | null;
  }>({ threadKey: activeThreadKey, messageId: null });
  if (timelineAnchor.threadKey !== activeThreadKey) {
    setTimelineAnchor({ threadKey: activeThreadKey, messageId: null });
  }
  const timelineAnchorMessageId = timelineAnchor.messageId;
  const activeRightPanelKind = useRightPanelStore((state) =>
    selectActiveRightPanel(state.byThreadKey, activeThreadRef),
  );
  const diffOpen = activeRightPanelKind === "diff";
  const rightPanelState = useRightPanelStore((state) =>
    selectThreadRightPanelState(state.byThreadKey, activeThreadRef),
  );
  const activeRightPanelSurface = useRightPanelStore((state) =>
    selectActiveRightPanelSurface(state.byThreadKey, activeThreadRef),
  );
  const persistedActivitySurface =
    rightPanelState.surfaces.find((surface) => surface.kind === "activity") ?? null;
  const persistedActivitySurfaceId = persistedActivitySurface?.id ?? null;
  const persistedActivityScopeTag = persistedActivitySurface?.scope._tag ?? null;
  const persistedActivityTerminalId =
    persistedActivitySurface?.scope._tag === "terminal"
      ? persistedActivitySurface.scope.terminalId
      : null;
  const activeActivitySurface =
    activeRightPanelSurface?.kind === "activity" ? activeRightPanelSurface : null;
  const activityScope = useMemo(
    () => resolveActivityScope(activeThreadRef, activeActivitySurface?.scope ?? { _tag: "thread" }),
    [activeActivitySurface?.scope, activeThreadRef],
  );
  const activityScopeEnabled =
    activityScope !== null &&
    isAgentActivityScopeEnabled(activityScope, {
      enableChatAgentActivity,
      enableTerminalAgentActivity,
    });
  const activityStateTarget = useMemo<ActivityStateTarget | null>(
    () =>
      activityScopeEnabled &&
      activeThreadRef !== null &&
      activityScope !== null &&
      (isPanel || !siblingChatOwnsCenter)
        ? { environmentId: activeThreadRef.environmentId, input: activityScope }
        : null,
    [activeThreadRef, activityScope, activityScopeEnabled, isPanel, siblingChatOwnsCenter],
  );
  useEffect(() => {
    if (
      activeThreadRef === null ||
      persistedActivitySurfaceId === null ||
      persistedActivityScopeTag === null
    ) {
      return;
    }
    const persistedScope =
      persistedActivityScopeTag === "terminal"
        ? persistedActivityTerminalId === null
          ? null
          : resolveActivityScope(activeThreadRef, {
              _tag: "terminal",
              terminalId: persistedActivityTerminalId,
            })
        : resolveActivityScope(activeThreadRef, { _tag: "thread" });
    if (
      persistedScope !== null &&
      !isAgentActivityScopeEnabled(persistedScope, {
        enableChatAgentActivity,
        enableTerminalAgentActivity,
      })
    ) {
      useRightPanelStore.getState().closeSurface(activeThreadRef, persistedActivitySurfaceId);
    }
  }, [
    activeThreadRef,
    enableChatAgentActivity,
    enableTerminalAgentActivity,
    persistedActivityScopeTag,
    persistedActivitySurfaceId,
    persistedActivityTerminalId,
  ]);
  const panelActivitySurfaceOpen =
    isPanel &&
    rightPanelState.isOpen &&
    activeActivitySurface !== null &&
    activityStateTarget !== null;
  const hostActivitySurfaceSuppressed =
    !isPanel && siblingChatOwnsCenter && activeActivitySurface !== null;
  const rightPanelSurfaceEligible = isPanel
    ? panelActivitySurfaceOpen
    : !hostActivitySurfaceSuppressed;
  const renderedRightPanelSurfaces =
    isPanel && activeActivitySurface !== null ? [activeActivitySurface] : rightPanelState.surfaces;
  const activeFileSurface =
    activeRightPanelSurface?.kind === "file" ? activeRightPanelSurface : null;
  const activePreviewState = useThreadPreviewState(activeThreadRef);
  const panelTerminalIds = useMemo(
    () =>
      new Set(
        rightPanelState.surfaces.flatMap((surface) =>
          surface.kind === "terminal" ? surface.terminalIds : [],
        ),
      ),
    [rightPanelState.surfaces],
  );
  // Boolean() tells React Compiler this is a primitive rather than a value that
  // may alias the center panel state.
  const hasTerminalSurface = Boolean(
    centerPanelState.surfaces.some((surface) => surface.kind === "terminal") ||
    panelTerminalIds.size > 0,
  );
  const previewPanelOpen = activeRightPanelKind === "preview" && isPreviewSupportedInRuntime();
  const rightPanelOpen = rightPanelState.isOpen;
  const effectiveRightPanelOpen = rightPanelOpen && rightPanelSurfaceEligible;
  const canMaximizeRightPanel = effectiveRightPanelOpen && !shouldUsePlanSidebarSheet;
  const rightPanelMaximized =
    canMaximizeRightPanel && maximizedRightPanelThreadKey === routeThreadKey;

  useEffect(() => {
    if (!activeThreadRef) return;
    useRightPanelStore
      .getState()
      .reconcileBrowserSurfaces(activeThreadRef, Object.keys(activePreviewState.sessions));
  }, [activePreviewState.sessions, activeThreadRef]);

  const planSidebarOpen = activeRightPanelKind === "plan";

  const activeLatestTurn = activeThread?.latestTurn ?? null;
  const sourcePlanThreadRef = useMemo(() => {
    const sourceThreadId = activeLatestTurn?.sourceProposedPlan?.threadId;
    if (!activeThread || !sourceThreadId || sourceThreadId === activeThread.id) {
      return null;
    }
    return scopeThreadRef(activeThread.environmentId, sourceThreadId);
  }, [activeLatestTurn?.sourceProposedPlan?.threadId, activeThread]);
  const sourceThreadProposedPlans = useThreadProposedPlans(sourcePlanThreadRef);
  const threadPlanCatalog = useMemo<ThreadPlanCatalogEntry[]>(() => {
    if (!activeThread) {
      return [];
    }
    const entries: ThreadPlanCatalogEntry[] = [
      { id: activeThread.id, proposedPlans: activeThread.proposedPlans },
    ];
    if (sourcePlanThreadRef) {
      entries.push({
        id: sourcePlanThreadRef.threadId,
        proposedPlans: sourceThreadProposedPlans,
      });
    }
    return entries;
  }, [activeThread, sourcePlanThreadRef, sourceThreadProposedPlans]);
  const latestTurnSettled = isLatestTurnSettled(activeLatestTurn, activeThread?.session ?? null);
  const activeProjectRef = activeThread
    ? scopeProjectRef(activeThread.environmentId, activeThread.projectId)
    : null;
  const activeProject = useProject(activeProjectRef);
  const activeEnvironmentDescriptor =
    activeThread === undefined
      ? undefined
      : environmentById.get(activeThread.environmentId)?.serverConfig?.environment;
  const activeEnvironmentSupportsWorktreeCatalog =
    activeEnvironmentDescriptor !== undefined &&
    selectWorktreeCatalogCapabilityPolicy(activeEnvironmentDescriptor).catalogRpc === "enabled";
  const activeWorktreeCatalog = useEnvironmentQuery(
    !isPanel &&
      activeThread?.worktreePath &&
      activeProject &&
      activeEnvironmentSupportsWorktreeCatalog
      ? worktreeEnvironment.catalog({
          environmentId: activeThread.environmentId,
          input: { projectId: activeProject.id },
        })
      : null,
  );
  const activeAdoptedWorkspace =
    activeThread === undefined
      ? null
      : (activeWorktreeCatalog.data?.adoptedWorkspaces.find(
          (workspace) => workspace.threadId === activeThread.id,
        ) ?? null);
  const workspaceUnavailable = isPanel
    ? props.workspaceUnavailable
    : selectWorktreeWorkspaceActionsAvailable(activeAdoptedWorkspace)
      ? null
      : WORKSPACE_UNAVAILABLE_REASON;
  const workspaceUnavailableRef = useRef(workspaceUnavailable);
  useLayoutEffect(() => {
    workspaceUnavailableRef.current = workspaceUnavailable;
  }, [workspaceUnavailable]);
  const readWorkspaceUnavailable = useCallback(() => workspaceUnavailableRef.current, []);
  const consumeActivityDockEscapeClose = useCallback(() => {
    if (!activeThread) {
      return false;
    }
    const projectKey = scopedProjectKey(
      scopeProjectRef(activeThread.environmentId, activeThread.projectId),
    );
    const state = useActivityDockStore.getState();
    if (!selectActivityDockExpanded(state.expandedByProject, projectKey)) {
      return false;
    }
    state.setExpanded(projectKey, false);
    return !selectActivityDockExpanded(
      useActivityDockStore.getState().expandedByProject,
      projectKey,
    );
  }, [activeThread]);
  const activeEnvironmentShell = useEnvironmentQuery(
    activeThread ? environmentShell.stateAtom(activeThread.environmentId) : null,
  );
  const activeEnvironmentBootstrapComplete = activeEnvironmentShell.data?.snapshot._tag === "Some";
  const activeProjectKey = activeProject
    ? JSON.stringify([
        activeProject.environmentId,
        activeProject.id,
        activeThread?.worktreePath ?? activeProject.workspaceRoot,
      ])
    : null;
  // Each project workspace gets its own registry, replaced while rendering when
  // the workspace changes.
  const [fileEditingSessionsByProject, setFileEditingSessionsByProject] = useState(() => ({
    projectKey: activeProjectKey,
    registry: new FileEditingSessionRegistry<FileEditingSession<FileCommentAnnotationGroup>>(),
  }));
  if (fileEditingSessionsByProject.projectKey !== activeProjectKey) {
    setFileEditingSessionsByProject({
      projectKey: activeProjectKey,
      registry: new FileEditingSessionRegistry<FileEditingSession<FileCommentAnnotationGroup>>(),
    });
  }
  const fileEditingSessions = fileEditingSessionsByProject.registry;
  const openFileRelativePaths = useMemo(
    () =>
      rightPanelState.surfaces.flatMap((surface) =>
        surface.kind === "file" ? [surface.relativePath] : [],
      ),
    [rightPanelState.surfaces],
  );
  const activeFileRelativePath =
    effectiveRightPanelOpen && activeRightPanelSurface?.kind === "file"
      ? activeRightPanelSurface.relativePath
      : null;

  useEffect(() => {
    void fileEditingSessions.reconcile(openFileRelativePaths);
  }, [fileEditingSessions, openFileRelativePaths]);

  useEffect(() => {
    fileEditingSessions.setSavingEnabled(workspaceUnavailable === null);
    fileEditingSessions.setActivePath(activeFileRelativePath);
  }, [activeFileRelativePath, fileEditingSessions, workspaceUnavailable]);

  useEffect(() => fileEditingSessions.acquireOwnership(), [fileEditingSessions]);
  const [pendingFileSurfaceIdsByProject, setPendingFileSurfaceIdsByProject] = useState<
    ReadonlyMap<string, ReadonlySet<string>>
  >(() => new Map());
  const pendingFileSurfaceIds = activeProjectKey
    ? (pendingFileSurfaceIdsByProject.get(activeProjectKey) ?? EMPTY_PENDING_FILE_SURFACE_IDS)
    : EMPTY_PENDING_FILE_SURFACE_IDS;
  const handleFilePendingChange = useCallback(
    (relativePath: string, pending: boolean) => {
      if (!activeProjectKey) return;
      setPendingFileSurfaceIdsByProject((currentByProject) => {
        const current = currentByProject.get(activeProjectKey) ?? EMPTY_PENDING_FILE_SURFACE_IDS;
        const surfaceId = `file:${relativePath}`;
        if (current.has(surfaceId) === pending) return currentByProject;
        const next = new Set(current);
        if (pending) next.add(surfaceId);
        else next.delete(surfaceId);
        const nextByProject = new Map(currentByProject);
        if (next.size === 0) nextByProject.delete(activeProjectKey);
        else nextByProject.set(activeProjectKey, next);
        return nextByProject;
      });
    },
    [activeProjectKey],
  );
  const configuredPreviewUrls = useMemo(
    () => getConfiguredPreviewUrls(activeProject?.scripts),
    [activeProject?.scripts],
  );

  useEffect(() => {
    if (!activeThreadRef || !activeEnvironmentBootstrapComplete) return;
    useRightPanelStore.getState().reconcileFileSurfaces(activeThreadRef, activeProject !== null);
  }, [activeEnvironmentBootstrapComplete, activeProject, activeThreadRef]);

  const activeEnvironment =
    activeThread == null ? null : (environmentById.get(activeThread.environmentId) ?? null);
  const activeEnvironmentUnavailable =
    activeEnvironment !== null && isConnectionUnavailable(activeEnvironment.connection);
  const activeEnvironmentUnavailableLabel = activeEnvironment?.label ?? null;
  const activeEnvironmentUnavailableState = useMemo<EnvironmentUnavailableState | null>(() => {
    if (!activeEnvironmentUnavailable || !activeEnvironmentUnavailableLabel || !activeEnvironment) {
      return null;
    }

    return {
      environmentId: activeEnvironment.environmentId,
      label: activeEnvironmentUnavailableLabel,
      connection: activeEnvironment.connection,
    };
  }, [activeEnvironment, activeEnvironmentUnavailable, activeEnvironmentUnavailableLabel]);
  const handleReconnectActiveEnvironment = useCallback(
    async (environmentId: EnvironmentId) => {
      const result = await retryEnvironment(environmentId);
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not reconnect environment",
            description: error instanceof Error ? error.message : "Failed to reconnect.",
          }),
        );
      }
    },
    [retryEnvironment],
  );
  const projectGroupingSettings = selectProjectGroupingSettings(settings);

  const closePullRequestDialog = useCallback(() => {
    setPullRequestDialogState(null);
  }, []);

  const openOrReuseProjectDraftThread = useCallback(
    async (input: { branch: string; worktreePath: string | null; envMode: DraftThreadEnvMode }) => {
      if (!activeProject) {
        throw new Error("No active project is available for this pull request.");
      }
      const activeProjectRef = scopeProjectRef(activeProject.environmentId, activeProject.id);
      const logicalProjectKey = deriveLogicalProjectKeyFromSettings(
        activeProject,
        projectGroupingSettings,
      );
      const storedDraftSession = getDraftSessionByLogicalProjectKey(
        logicalProjectKey,
        projectDraftFallbackKeys(activeProject, logicalProjectKey),
        activeProjectRef,
      );
      if (storedDraftSession) {
        setDraftThreadContext(storedDraftSession.draftId, input);
        setLogicalProjectDraftThreadId(
          logicalProjectKey,
          activeProjectRef,
          storedDraftSession.draftId,
          {
            threadId: storedDraftSession.threadId,
            ...input,
          },
        );
        if (routeKind !== "draft" || draftId !== storedDraftSession.draftId) {
          await navigate({
            to: "/draft/$draftId",
            params: buildDraftThreadRouteParams(storedDraftSession.draftId),
          });
        }
        return storedDraftSession.threadId;
      }

      const activeDraftSession = routeKind === "draft" && draftId ? getDraftSession(draftId) : null;
      if (
        !isServerThread &&
        activeDraftSession?.logicalProjectKey === logicalProjectKey &&
        draftId
      ) {
        setDraftThreadContext(draftId, input);
        setLogicalProjectDraftThreadId(logicalProjectKey, activeProjectRef, draftId, {
          threadId: activeDraftSession.threadId,
          createdAt: activeDraftSession.createdAt,
          runtimeMode: activeDraftSession.runtimeMode,
          interactionMode: activeDraftSession.interactionMode,
          ...input,
        });
        return activeDraftSession.threadId;
      }

      const nextDraftId = newDraftId();
      const nextThreadId = newThreadId();
      const targetInstanceId =
        activeProject.defaultModelSelection?.instanceId ??
        useComposerDraftStore.getState().stickyActiveProvider ??
        defaultInstanceIdForDriver(ProviderDriverKind.make("codex"));
      const resolution = resolveProviderSessionSelectionForInstance({
        instanceId: targetInstanceId,
        providers: activeEnvironment?.serverConfig?.providers ?? [],
        settings: {
          providerInstances: settings.providerInstances,
          providerSessionDefaults: settings.providerSessionDefaults,
        },
        projectSelection: activeProject.defaultModelSelection,
      });
      setLogicalProjectDraftThreadId(logicalProjectKey, activeProjectRef, nextDraftId, {
        threadId: nextThreadId,
        createdAt: new Date().toISOString(),
        runtimeMode: DEFAULT_RUNTIME_MODE,
        interactionMode: DEFAULT_INTERACTION_MODE,
        ...input,
      });
      setComposerDraftModelSelection(nextDraftId, resolution.modelSelection);
      if (resolution.fallback) {
        console.warn("Provider session default fallback", resolution.fallback);
      }
      await navigate({
        to: "/draft/$draftId",
        params: buildDraftThreadRouteParams(nextDraftId),
      });
      return nextThreadId;
    },
    [
      activeProject,
      activeEnvironment?.serverConfig?.providers,
      draftId,
      getDraftSession,
      getDraftSessionByLogicalProjectKey,
      isServerThread,
      navigate,
      projectGroupingSettings,
      routeKind,
      setComposerDraftModelSelection,
      setDraftThreadContext,
      setLogicalProjectDraftThreadId,
      settings.providerInstances,
      settings.providerSessionDefaults,
    ],
  );

  const handlePreparedPullRequestThread = useCallback(
    async (input: { branch: string; mode: "local" | "worktree" }) => {
      if (input.mode === "local") {
        await openOrReuseProjectDraftThread({
          branch: input.branch,
          worktreePath: null,
          envMode: "local",
        });
        return;
      }
      if (!activeProject) {
        throw new Error("No active project is available for this pull request.");
      }
      const nextThreadId = newThreadId();
      const targetInstanceId =
        activeProject.defaultModelSelection?.instanceId ??
        useComposerDraftStore.getState().stickyActiveProvider ??
        defaultInstanceIdForDriver(ProviderDriverKind.make("codex"));
      const resolution = resolveProviderSessionSelectionForInstance({
        instanceId: targetInstanceId,
        providers: activeEnvironment?.serverConfig?.providers ?? [],
        settings,
        projectSelection: activeProject.defaultModelSelection,
      });
      const result = await createManagedWorktree({
        environmentId: activeProject.environmentId,
        input: {
          commandId: newCommandId(),
          projectId: activeProject.id,
          threadId: nextThreadId,
          title: input.branch,
          refName: input.branch,
          newRefName: null,
          baseRefName: null,
          threadDefaults: {
            modelSelection: resolution.modelSelection,
            runtimeMode: DEFAULT_RUNTIME_MODE,
            interactionMode: DEFAULT_INTERACTION_MODE,
          },
        },
      });
      if (result._tag === "Failure") {
        throw squashAtomCommandFailure(result);
      }
      if (resolution.fallback) {
        console.warn("Provider session default fallback", resolution.fallback);
      }
      await navigate({
        to: "/$environmentId/$threadId",
        params: {
          environmentId: activeProject.environmentId,
          threadId: nextThreadId,
        },
      });
    },
    [
      activeEnvironment?.serverConfig?.providers,
      activeProject,
      createManagedWorktree,
      navigate,
      openOrReuseProjectDraftThread,
      settings,
    ],
  );

  useEffect(() => {
    // Panel variant: sidebar visited-state is host-owned; a panel must not touch it.
    if (isPanel) return;
    if (!serverThread?.id) return;
    const threadUpdatedAt = Date.parse(serverThread.updatedAt);
    if (Number.isNaN(threadUpdatedAt)) return;
    const lastVisitedAt = activeThreadLastVisitedAt ? Date.parse(activeThreadLastVisitedAt) : NaN;
    if (!Number.isNaN(lastVisitedAt) && lastVisitedAt >= threadUpdatedAt) return;

    markThreadVisited(
      scopedThreadKey(scopeThreadRef(serverThread.environmentId, serverThread.id)),
      serverThread.updatedAt,
    );
  }, [
    isPanel,
    activeThreadLastVisitedAt,
    markThreadVisited,
    serverThread?.environmentId,
    serverThread?.id,
    serverThread?.updatedAt,
  ]);

  const selectedProviderByThreadId = composerActiveProvider ?? null;
  // Once a thread selects an environment, never substitute the primary
  // environment's config while the selected environment is still loading.
  const serverConfig = activeThread
    ? (activeEnvironment?.serverConfig ?? null)
    : (primaryEnvironment?.serverConfig ?? null);
  const providerStatuses = serverConfig?.providers ?? EMPTY_PROVIDERS;
  const providerInstanceEntries = useMemo(
    () => applyProviderInstanceSettings(deriveProviderInstanceEntries(providerStatuses), settings),
    [providerStatuses, settings],
  );
  const instanceLabels = useMemo<ReadonlyMap<ProviderInstanceId, string>>(
    () => new Map(providerInstanceEntries.map((entry) => [entry.instanceId, entry.displayName])),
    [providerInstanceEntries],
  );
  const providerBinding = resolveThreadProviderBinding({
    thread: activeThread,
    projectDefaultModelSelection: activeProject?.defaultModelSelection,
    selectedProviderInstanceId: selectedProviderByThreadId,
    providers: providerStatuses,
  });
  const providerBindingConflictReason = providerBinding.conflict?.reason ?? null;
  const versionMismatch = resolveServerConfigVersionMismatch(serverConfig);
  const versionMismatchDismissKey =
    versionMismatch && activeThread
      ? buildVersionMismatchDismissalKey(activeThread.environmentId, versionMismatch)
      : null;
  const [dismissedVersionMismatchKey, setDismissedVersionMismatchKey] = useState<string | null>(
    null,
  );
  const [resolvingTurnDeliveryMessageId, setResolvingTurnDeliveryMessageId] =
    useState<MessageId | null>(null);
  const versionMismatchDismissed =
    versionMismatchDismissKey === dismissedVersionMismatchKey ||
    isVersionMismatchDismissed(versionMismatchDismissKey);
  const showVersionMismatchBanner =
    versionMismatch !== null && versionMismatchDismissKey !== null && !versionMismatchDismissed;
  const hasMultipleRegisteredEnvironments = environments.length > 1;
  const versionMismatchServerLabel =
    hasMultipleRegisteredEnvironments && activeThread
      ? `${environmentById.get(activeThread.environmentId)?.label ?? serverConfig?.environment.label ?? activeThread.environmentId} server`
      : "server";
  const composerBannerItems = useMemo<ComposerBannerStackItem[]>(() => {
    const items: ComposerBannerStackItem[] = [];
    if (activeEnvironmentUnavailableState) {
      const connection = activeEnvironmentUnavailableState.connection;
      const target = environmentById.get(activeEnvironmentUnavailableState.environmentId)?.entry
        ?.target;
      const connectionActions = environmentConnectionActions(presentation, target);
      const permitsReconnect = connectionActions.reconnect;
      const showConnections = presentation.showRemoteDeviceControls;
      const isReconnecting =
        connection.phase === "connecting" || connection.phase === "reconnecting";
      items.push({
        id: `environment-unavailable:${activeEnvironmentUnavailableState.environmentId}`,
        variant: connection.phase === "error" ? "error" : "warning",
        icon: <WifiOffIcon />,
        ...describeUnavailableEnvironment(activeEnvironmentUnavailableState),
        actions:
          permitsReconnect || connectionActions.openRemoteServers || showConnections ? (
            <>
              {connectionActions.openRemoteServers ? (
                <Button size="xs" onClick={() => void navigate({ to: "/settings/remote-servers" })}>
                  Open Remote Servers
                </Button>
              ) : null}
              {permitsReconnect ? (
                <Button
                  size="xs"
                  disabled={isReconnecting}
                  onClick={() =>
                    void handleReconnectActiveEnvironment(
                      activeEnvironmentUnavailableState.environmentId,
                    )
                  }
                >
                  {isReconnecting ? "Reconnecting..." : "Reconnect"}
                </Button>
              ) : null}
              {showConnections ? (
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => void navigate({ to: "/settings/remote-servers" })}
                >
                  Remote Servers
                </Button>
              ) : null}
            </>
          ) : undefined,
      });
    }
    if (workspaceUnavailable) {
      items.push({
        id: `workspace-unavailable:${activeThread?.id ?? "unknown"}`,
        variant: "warning",
        icon: <TriangleAlertIcon />,
        title: "Workspace unavailable",
        description: workspaceUnavailable,
      });
    }
    if (providerBinding.conflict) {
      items.push({
        id: `provider-binding-conflict:${providerBinding.conflict.instanceId}`,
        variant: "warning",
        icon: <TriangleAlertIcon />,
        title: "Provider session conflict",
        description: providerBinding.conflict.reason,
      });
    }
    if (showVersionMismatchBanner && versionMismatch && versionMismatchDismissKey) {
      items.push({
        id: `version-mismatch:${versionMismatchDismissKey}`,
        variant: "warning",
        icon: <TriangleAlertIcon />,
        title: "Client and server versions differ",
        description: (
          <>
            Client {versionMismatch.clientVersion} is connected to {versionMismatchServerLabel}{" "}
            {versionMismatch.serverVersion}. Sync them if RPC calls or reconnects fail.
          </>
        ),
        dismissLabel: "Dismiss version mismatch warning",
        onDismiss: () => {
          dismissVersionMismatch(versionMismatchDismissKey);
          setDismissedVersionMismatchKey(versionMismatchDismissKey);
        },
      });
    }
    return items;
  }, [
    activeEnvironmentUnavailableState,
    activeThread?.id,
    handleReconnectActiveEnvironment,
    environmentById,
    navigate,
    presentation,
    providerBinding.conflict,
    setDismissedVersionMismatchKey,
    showVersionMismatchBanner,
    versionMismatch,
    versionMismatchDismissKey,
    versionMismatchServerLabel,
    workspaceUnavailable,
  ]);
  const { lockedProvider, lockedProviderInstanceId } = providerBinding;
  const lockProviderPickerToActiveInstance = isPanel || lockedProviderInstanceId !== null;
  const unlockedSelectedProvider = resolveSelectableProvider(
    providerStatuses,
    providerBinding.instanceId,
  );
  const selectedProvider: ProviderDriverKind =
    lockedProvider ?? providerBinding.driver ?? unlockedSelectedProvider;
  const phase = derivePhase(activeThread?.session ?? null);
  const threadActivities = activeThread?.activities ?? EMPTY_ACTIVITIES;
  const workLogEntries = useMemo(() => deriveWorkLogEntries(threadActivities), [threadActivities]);
  const pendingApprovals = useMemo(
    () => derivePendingApprovals(threadActivities),
    [threadActivities],
  );
  const pendingUserInputs = useMemo(
    () => derivePendingUserInputs(threadActivities),
    [threadActivities],
  );
  const activePendingUserInput = pendingUserInputs[0] ?? null;
  const activePendingDraftAnswers = useMemo(
    () =>
      activePendingUserInput
        ? (pendingUserInputAnswersByRequestId[activePendingUserInput.requestId] ??
          EMPTY_PENDING_USER_INPUT_ANSWERS)
        : EMPTY_PENDING_USER_INPUT_ANSWERS,
    [activePendingUserInput, pendingUserInputAnswersByRequestId],
  );
  const activePendingQuestionIndex = activePendingUserInput
    ? (pendingUserInputQuestionIndexByRequestId[activePendingUserInput.requestId] ?? 0)
    : 0;
  const activePendingProgress = useMemo(
    () =>
      activePendingUserInput
        ? derivePendingUserInputProgress(
            activePendingUserInput.questions,
            activePendingDraftAnswers,
            activePendingQuestionIndex,
          )
        : null,
    [activePendingDraftAnswers, activePendingQuestionIndex, activePendingUserInput],
  );
  const activePendingResolvedAnswers = useMemo(
    () =>
      activePendingUserInput
        ? buildPendingUserInputAnswers(activePendingUserInput.questions, activePendingDraftAnswers)
        : null,
    [activePendingDraftAnswers, activePendingUserInput],
  );
  const activePendingIsResponding = activePendingUserInput
    ? respondingUserInputRequestIds.includes(activePendingUserInput.requestId)
    : false;
  const activeProposedPlan = useMemo(() => {
    if (!latestTurnSettled) {
      return null;
    }
    return findLatestProposedPlan(
      activeThread?.proposedPlans ?? [],
      activeLatestTurn?.turnId ?? null,
    );
  }, [activeLatestTurn?.turnId, activeThread?.proposedPlans, latestTurnSettled]);
  const sidebarProposedPlan = useMemo(
    () =>
      findSidebarProposedPlan({
        threads: threadPlanCatalog,
        latestTurn: activeLatestTurn,
        latestTurnSettled,
        threadId: activeThread?.id ?? null,
      }),
    [activeLatestTurn, activeThread?.id, latestTurnSettled, threadPlanCatalog],
  );
  const activePlan = useMemo(
    () => deriveActivePlanState(threadActivities, activeLatestTurn?.turnId ?? undefined),
    [activeLatestTurn?.turnId, threadActivities],
  );
  const planSidebarLabel = sidebarProposedPlan || interactionMode === "plan" ? "Plan" : "Tasks";
  const showPlanFollowUpPrompt =
    pendingUserInputs.length === 0 &&
    interactionMode === "plan" &&
    latestTurnSettled &&
    hasActionableProposedPlan(activeProposedPlan);
  const activePendingApproval = pendingApprovals[0] ?? null;
  const {
    beginLocalDispatch,
    resetLocalDispatch,
    localDispatchStartedAt,
    activeDeliveryStartedAt,
    cancellableDeliveryThreadId,
    cancellableDeliveryMessageId,
    canCancelPendingSend,
    isPreparingWorktree,
    isSendBusy,
    isSendActivelyWorking,
  } = useLocalDispatchState({
    activeThread,
    activeLatestTurn,
    phase,
    activePendingApproval: activePendingApproval?.requestId ?? null,
    activePendingUserInput: activePendingUserInput?.requestId ?? null,
    localError: isServerThread ? localServerError : localDraftError,
  });
  const queuedMessages = useMemo(
    () => selectQueuedMessages(activeThread?.messages ?? []),
    [activeThread?.messages],
  );
  const blocker = useMemo(
    () => findBlockingDelivery(activeThread?.messages ?? []),
    [activeThread?.messages],
  );
  const blockingId = blocker?.id ?? null;
  const blockingCreatedAt = blocker?.createdAt ?? null;
  const blockingOffersRetry = blocker?.delivery ? deliveryOffersRetry(blocker.delivery) : true;
  const blockingDelivery = useMemo(
    () =>
      blockingId !== null && blockingCreatedAt !== null
        ? { id: blockingId, createdAt: blockingCreatedAt, offersRetry: blockingOffersRetry }
        : null,
    [blockingId, blockingCreatedAt, blockingOffersRetry],
  );
  const supportsTurnSteer = providerBinding.status?.supportsTurnSteer === true;
  const hasPendingApproval = activePendingApproval !== null;
  const hasPendingUserInput = activePendingUserInput !== null;
  const queueSessionStatus = activeThread?.session?.status ?? null;
  const queuedStatuses = useMemo(() => {
    return queuedMessages.map((message, index) =>
      deriveQueuedCardStatus({
        index,
        phase,
        sessionStatus: queueSessionStatus,
        supportsTurnSteer,
        delivery: message.delivery!,
        hasPendingApproval,
        hasPendingUserInput,
        waitingBehind:
          blockingDelivery !== null && waitsBehind(message, blockingDelivery)
            ? { offersRetry: blockingDelivery.offersRetry }
            : null,
      }),
    );
  }, [
    queuedMessages,
    phase,
    queueSessionStatus,
    supportsTurnSteer,
    hasPendingApproval,
    hasPendingUserInput,
    blockingDelivery,
  ]);
  const [resolvingQueuedMessageId, setResolvingQueuedMessageId] = useState<MessageId | null>(null);
  const [queuedMessageErrors, setQueuedMessageErrors] = useState<Record<string, string>>({});
  const queuedActionsInFlightRef = useRef(new Set<MessageId>());
  const stoppingQueueRef = useRef(false);
  const currentQueueThreadKeyRef = useRef<string | null>(routeThreadKey);
  useLayoutEffect(() => {
    currentQueueThreadKeyRef.current = routeThreadKey;
    return () => {
      currentQueueThreadKeyRef.current = null;
    };
  }, [routeThreadKey]);
  useEffect(
    () => () => {
      for (const attempt of attachmentSendControllers.current.values()) {
        if (attempt.threadKey !== routeThreadKey) continue;
        if (attempt.phase === "staging") {
          attempt.controller.abort();
          attempt.restore();
        }
      }
      const retained = stagedAttachmentAttempts.get(routeThreadRef);
      const admitting = [...attachmentSendControllers.current.values()].some(
        (attempt) => attempt.threadKey === routeThreadKey && attempt.phase === "admitting",
      );
      if (
        retained &&
        !retained.ambiguousAdmission &&
        !admitting &&
        !stagedAttachmentAttempts.isBusy(routeThreadRef)
      ) {
        stagedAttachmentAttempts.delete(routeThreadRef);
        void releaseStagedAttachments({ environmentId, input: retained.attachments });
      }
    },
    [environmentId, releaseStagedAttachments, routeThreadKey, routeThreadRef],
  );
  const previousQueueMessagesRef = useRef({ threadKey: routeThreadKey, messages: queuedMessages });
  useEffect(() => {
    const previous = previousQueueMessagesRef.current;
    if (previous.threadKey === routeThreadKey) {
      const currentIds = new Set(queuedMessages.map((message) => message.id));
      for (const message of previous.messages) {
        if (!currentIds.has(message.id)) queuedMessageCache.forget(message.id);
      }
    }
    for (const message of activeThread?.messages ?? []) {
      if (message.delivery?.state === "delivered" || message.delivery?.state === "dismissed") {
        queuedMessageCache.forget(message.id);
      }
    }
    previousQueueMessagesRef.current = { threadKey: routeThreadKey, messages: queuedMessages };
  }, [activeThread?.messages, queuedMessages, routeThreadKey]);
  const isWorking =
    phase === "running" ||
    (isSendActivelyWorking && !hasActiveAttachmentUpload) ||
    isConnecting ||
    isRevertingCheckpoint;
  const sendStartedAt = localDispatchStartedAt ?? activeDeliveryStartedAt;
  const activeWorkStartedAt =
    phase !== "running" && sendStartedAt !== null
      ? sendStartedAt
      : deriveActiveWorkStartedAt(activeLatestTurn, activeThread?.session ?? null, sendStartedAt);
  useEffect(() => {
    attachmentPreviewHandoffByMessageIdRef.current = attachmentPreviewHandoffByMessageId;
  }, [attachmentPreviewHandoffByMessageId]);
  const clearAttachmentPreviewHandoff = useCallback(
    (messageId: MessageId, previewUrls?: ReadonlyArray<string>) => {
      delete attachmentPreviewPromotionInFlightByMessageIdRef.current[messageId];
      const currentPreviewUrls =
        previewUrls ?? attachmentPreviewHandoffByMessageIdRef.current[messageId] ?? [];
      setAttachmentPreviewHandoffByMessageId((existing) => {
        if (!(messageId in existing)) {
          return existing;
        }
        const next = { ...existing };
        delete next[messageId];
        attachmentPreviewHandoffByMessageIdRef.current = next;
        return next;
      });
      for (const previewUrl of currentPreviewUrls) {
        revokeBlobPreviewUrl(previewUrl);
      }
    },
    [],
  );
  const clearAttachmentPreviewHandoffs = useCallback(() => {
    attachmentPreviewPromotionInFlightByMessageIdRef.current = {};
    for (const previewUrls of Object.values(attachmentPreviewHandoffByMessageIdRef.current)) {
      for (const previewUrl of previewUrls) {
        revokeBlobPreviewUrl(previewUrl);
      }
    }
    attachmentPreviewHandoffByMessageIdRef.current = {};
    setAttachmentPreviewHandoffByMessageId({});
  }, []);
  useEffect(() => {
    return () => {
      clearAttachmentPreviewHandoffs();
      for (const message of optimisticUserMessagesRef.current) {
        revokeUserMessagePreviewUrls(message);
      }
    };
  }, [clearAttachmentPreviewHandoffs]);
  const handoffAttachmentPreviews = useCallback((messageId: MessageId, previewUrls: string[]) => {
    if (previewUrls.length === 0) return;

    const previousPreviewUrls = attachmentPreviewHandoffByMessageIdRef.current[messageId] ?? [];
    const nextPreviewUrlSet = new Set(previewUrls);
    for (const previewUrl of previousPreviewUrls) {
      if (!nextPreviewUrlSet.has(previewUrl)) {
        revokeBlobPreviewUrl(previewUrl);
      }
    }
    setAttachmentPreviewHandoffByMessageId((existing) => {
      const next = {
        ...existing,
        [messageId]: previewUrls,
      };
      attachmentPreviewHandoffByMessageIdRef.current = next;
      return next;
    });
  }, []);
  const serverMessages = activeThread?.messages;
  const serverImageAttachmentIds = useMemo(() => {
    const attachmentIds = new Set<string>();
    for (const message of serverMessages ?? []) {
      for (const attachment of message.attachments ?? []) {
        if (attachment.type !== "image") continue;
        attachmentIds.add(attachment.id);
      }
    }
    return [...attachmentIds];
  }, [serverMessages]);
  const serverAttachmentResources = useMemo(
    () =>
      serverImageAttachmentIds.map((attachmentId) => ({
        _tag: "attachment" as const,
        attachmentId,
      })),
    [serverImageAttachmentIds],
  );
  const serverAttachmentUrls = useAssetUrls(environmentId, serverAttachmentResources);
  const serverAttachmentUrlById = useMemo(
    () =>
      new Map(
        serverImageAttachmentIds.flatMap((attachmentId, index) => {
          const url = serverAttachmentUrls[index];
          return url ? [[attachmentId, url] as const] : [];
        }),
      ),
    [serverImageAttachmentIds, serverAttachmentUrls],
  );
  const displayServerMessages = useMemo<ReadonlyArray<ChatMessage>>(() => {
    if (!serverMessages) return [];
    return serverMessages.map((message) => {
      if (!message.attachments || message.attachments.length === 0) {
        return message;
      }
      return {
        ...message,
        attachments: message.attachments.map((attachment) => {
          if (attachment.type !== "image") return attachment;
          const previewUrl = serverAttachmentUrlById.get(attachment.id);
          return previewUrl ? { ...attachment, previewUrl } : attachment;
        }),
      };
    });
  }, [serverAttachmentUrlById, serverMessages]);
  useEffect(() => {
    if (typeof Image === "undefined" || displayServerMessages.length === 0) {
      return;
    }

    const cleanups: Array<() => void> = [];
    const userMessagesById = new Map<string, ChatMessage>(
      displayServerMessages
        .filter((message) => message.role === "user")
        .map((message) => [String(message.id), message] as const),
    );

    for (const [messageId, handoffPreviewUrls] of Object.entries(
      attachmentPreviewHandoffByMessageId,
    )) {
      if (attachmentPreviewPromotionInFlightByMessageIdRef.current[messageId]) {
        continue;
      }

      const serverMessage = userMessagesById.get(messageId);
      if (!serverMessage?.attachments || serverMessage.attachments.length === 0) {
        continue;
      }

      const serverPreviewUrls = serverMessage.attachments.flatMap((attachment) =>
        attachment.type === "image" && attachment.previewUrl ? [attachment.previewUrl] : [],
      );
      if (
        serverPreviewUrls.length === 0 ||
        serverPreviewUrls.length !== handoffPreviewUrls.length ||
        serverPreviewUrls.some((previewUrl) => previewUrl.startsWith("blob:"))
      ) {
        continue;
      }

      attachmentPreviewPromotionInFlightByMessageIdRef.current[messageId] = true;

      let cancelled = false;
      const imageInstances: HTMLImageElement[] = [];

      const preloadServerPreviews = Promise.all(
        serverPreviewUrls.map(
          (previewUrl) =>
            new Promise<void>((resolve, reject) => {
              const image = new Image();
              imageInstances.push(image);
              const handleLoad = () => resolve();
              const handleError = () =>
                reject(new Error(`Failed to load server preview for ${messageId}.`));
              image.addEventListener("load", handleLoad, { once: true });
              image.addEventListener("error", handleError, { once: true });
              image.src = previewUrl;
            }),
        ),
      );

      void preloadServerPreviews
        .then(() => {
          if (cancelled) {
            return;
          }
          clearAttachmentPreviewHandoff(messageId as MessageId, handoffPreviewUrls);
        })
        .catch(() => {
          if (!cancelled) {
            delete attachmentPreviewPromotionInFlightByMessageIdRef.current[messageId];
          }
        });

      cleanups.push(() => {
        cancelled = true;
        delete attachmentPreviewPromotionInFlightByMessageIdRef.current[messageId];
        for (const image of imageInstances) {
          image.src = "";
        }
      });
    }

    return () => {
      for (const cleanup of cleanups) {
        cleanup();
      }
    };
  }, [attachmentPreviewHandoffByMessageId, clearAttachmentPreviewHandoff, displayServerMessages]);
  const timelineMessages = useMemo(() => {
    const messages = displayServerMessages;
    const serverMessagesWithPreviewHandoff =
      Object.keys(attachmentPreviewHandoffByMessageId).length === 0
        ? messages
        : // Spread only fires for the few messages that actually changed;
          // unchanged ones early-return their original reference.
          // In-place mutation would break React's immutable state contract.
          messages.map((message) => {
            if (
              message.role !== "user" ||
              !message.attachments ||
              message.attachments.length === 0
            ) {
              return message;
            }
            const handoffPreviewUrls = attachmentPreviewHandoffByMessageId[message.id];
            if (!handoffPreviewUrls || handoffPreviewUrls.length === 0) {
              return message;
            }

            let changed = false;
            let imageIndex = 0;
            const attachments = message.attachments.map((attachment) => {
              if (attachment.type !== "image") {
                return attachment;
              }
              const handoffPreviewUrl = handoffPreviewUrls[imageIndex];
              imageIndex += 1;
              if (!handoffPreviewUrl || attachment.previewUrl === handoffPreviewUrl) {
                return attachment;
              }
              changed = true;
              return {
                ...attachment,
                previewUrl: handoffPreviewUrl,
              };
            });

            return changed ? { ...message, attachments } : message;
          });

    if (optimisticUserMessages.length === 0) {
      return serverMessagesWithPreviewHandoff;
    }
    const serverIds = new Set(serverMessagesWithPreviewHandoff.map((message) => message.id));
    const pendingMessages = optimisticUserMessages.filter((message) => !serverIds.has(message.id));
    if (pendingMessages.length === 0) {
      return serverMessagesWithPreviewHandoff;
    }
    return [...serverMessagesWithPreviewHandoff, ...pendingMessages];
  }, [attachmentPreviewHandoffByMessageId, displayServerMessages, optimisticUserMessages]);
  const timelineEntries = useMemo(
    () =>
      deriveTimelineEntries(
        timelineMessages.filter((message) => !isQueuedTimelineMessage(message)),
        activeThread?.proposedPlans ?? [],
        workLogEntries,
      ),
    [activeThread?.proposedPlans, timelineMessages, workLogEntries],
  );
  const { turnDiffSummaries, inferredCheckpointTurnCountByTurnId } =
    useTurnDiffSummaries(activeThread);
  const turnDiffSummaryByAssistantMessageId = useMemo(() => {
    const byMessageId = new Map<MessageId, TurnDiffSummary>();
    for (const summary of turnDiffSummaries) {
      if (!summary.assistantMessageId) continue;
      byMessageId.set(summary.assistantMessageId, summary);
    }
    return byMessageId;
  }, [turnDiffSummaries]);
  const revertTurnCountByUserMessageId = useMemo(() => {
    const byUserMessageId = new Map<MessageId, number>();
    for (let index = 0; index < timelineEntries.length; index += 1) {
      const entry = timelineEntries[index];
      if (!entry || entry.kind !== "message" || entry.message.role !== "user") {
        continue;
      }

      for (let nextIndex = index + 1; nextIndex < timelineEntries.length; nextIndex += 1) {
        const nextEntry = timelineEntries[nextIndex];
        if (!nextEntry || nextEntry.kind !== "message") {
          continue;
        }
        if (nextEntry.message.role === "user") {
          break;
        }
        const summary = turnDiffSummaryByAssistantMessageId.get(nextEntry.message.id);
        if (!summary) {
          continue;
        }
        const turnCount =
          summary.checkpointTurnCount ?? inferredCheckpointTurnCountByTurnId[summary.turnId];
        if (typeof turnCount !== "number") {
          break;
        }
        byUserMessageId.set(entry.message.id, Math.max(0, turnCount - 1));
        break;
      }
    }

    return byUserMessageId;
  }, [inferredCheckpointTurnCountByTurnId, timelineEntries, turnDiffSummaryByAssistantMessageId]);

  const gitCwd = activeProject
    ? projectScriptCwd({
        project: { cwd: activeProject.workspaceRoot },
        worktreePath: activeThread?.worktreePath ?? null,
      })
    : null;
  const gitStatusQuery = useEnvironmentQuery(
    gitCwd === null || workspaceUnavailable !== null
      ? null
      : vcsEnvironment.status({
          environmentId,
          input: { cwd: gitCwd },
        }),
  );
  const keybindings = useAtomValue(primaryServerKeybindingsAtom);
  const availableEditors = useAtomValue(primaryServerAvailableEditorsAtom);
  const activeProviderStatus = providerBinding.status;
  const centerHostLabel = activeProviderStatus
    ? activeProviderStatus.displayName?.trim() || providerDriverLabel(activeProviderStatus.driver)
    : lockedProviderInstanceId
      ? formatProviderSlugLabel(lockedProviderInstanceId)
      : providerDriverLabel(providerBinding.driver ?? selectedProvider);
  const activeProjectCwd = activeProject?.workspaceRoot ?? null;
  const activeThreadWorktreePath = activeThread?.worktreePath ?? null;
  const activeWorkspaceRoot = activeThreadWorktreePath ?? activeProjectCwd ?? undefined;
  const filePreviewViewKey =
    activeProject && activeWorkspaceRoot
      ? JSON.stringify([
          activeProject.environmentId,
          activeWorkspaceRoot,
          typeof composerDraftTarget === "string" ? "draft" : "thread",
          typeof composerDraftTarget === "string"
            ? composerDraftTarget
            : scopedThreadKey(composerDraftTarget),
        ])
      : null;
  // Default true while loading to avoid toolbar flicker.
  const isGitRepo = gitStatusQuery.data?.isRepo ?? true;
  const gitRightPanelAvailable = activeProject !== null && isGitRepo;
  const terminalShortcutLabelOptions = useMemo(
    () => ({
      context: {
        terminalFocus: true,
        terminalOpen: hasTerminalSurface,
      },
    }),
    [hasTerminalSurface],
  );
  const splitTerminalShortcutLabel = useMemo(
    () => shortcutLabelForCommand(keybindings, "terminal.split", terminalShortcutLabelOptions),
    [keybindings, terminalShortcutLabelOptions],
  );
  const splitTerminalVerticalShortcutLabel = useMemo(
    () =>
      shortcutLabelForCommand(keybindings, "terminal.splitVertical", terminalShortcutLabelOptions),
    [keybindings, terminalShortcutLabelOptions],
  );
  const newTerminalShortcutLabel = useMemo(
    () => shortcutLabelForCommand(keybindings, "terminal.new", terminalShortcutLabelOptions),
    [keybindings, terminalShortcutLabelOptions],
  );
  const closeTerminalShortcutLabel = useMemo(
    () => shortcutLabelForCommand(keybindings, "terminal.close", terminalShortcutLabelOptions),
    [keybindings, terminalShortcutLabelOptions],
  );
  const onToggleDiff = useCallback(() => {
    if (!isServerThread) {
      return;
    }
    if (!diffOpen) {
      onDiffPanelOpen?.();
    }
    if (activeThreadRef) {
      useRightPanelStore.getState().toggle(activeThreadRef, "diff");
    }
  }, [activeThreadRef, diffOpen, isServerThread, onDiffPanelOpen]);

  const envLocked = Boolean(
    activeThread &&
    (activeThread.messages.length > 0 ||
      (activeThread.session !== null && activeThread.session.status !== "stopped")),
  );

  const setThreadError = useCallback(
    (targetThreadId: ThreadId | null, error: string | null) => {
      if (!targetThreadId) return;
      const nextError = sanitizeThreadErrorMessage(error);
      if (
        serverThread &&
        targetThreadId === routeThreadRef.threadId &&
        serverThread.environmentId === routeThreadRef.environmentId &&
        serverThread.id === targetThreadId
      ) {
        setLocalServerErrorsByThreadKey((existing) => {
          if ((existing[routeThreadKey] ?? null) === nextError) {
            return existing;
          }
          return {
            ...existing,
            [routeThreadKey]: nextError,
          };
        });
        return;
      }
      const localDraftErrorKey = draftId ?? targetThreadId;
      setLocalDraftErrorsByDraftId((existing) => {
        if ((existing[localDraftErrorKey] ?? null) === nextError) {
          return existing;
        }
        return {
          ...existing,
          [localDraftErrorKey]: nextError,
        };
      });
    },
    [draftId, routeThreadKey, routeThreadRef, serverThread],
  );

  const focusComposer = useCallback(() => {
    readComposerHandle(composerRef)?.focusAtEnd();
  }, [composerRef]);
  const scheduleComposerFocus = useCallback(() => {
    window.requestAnimationFrame(() => {
      focusComposer();
    });
  }, [focusComposer]);
  const addTerminalContextToDraft = useCallback(
    (selection: TerminalContextSelection) => {
      readComposerHandle(composerRef)?.addTerminalContext(selection);
    },
    [composerRef],
  );
  const persistProjectScripts = useCallback(
    async (input: {
      projectId: ProjectId;
      projectCwd: string;
      previousScripts: ReadonlyArray<ProjectScript>;
      nextScripts: ReadonlyArray<ProjectScript>;
      keybinding?: string | null;
      keybindingCommand: KeybindingCommand;
    }): Promise<AtomCommandResult<void, unknown>> => {
      const updateResult = mapAtomCommandResult(
        await updateProject({
          environmentId,
          input: {
            projectId: input.projectId,
            scripts: input.nextScripts,
          },
        }),
        () => undefined,
      );
      if (updateResult._tag === "Failure") {
        return updateResult;
      }

      const keybindingRule = decodeProjectScriptKeybindingRule({
        keybinding: input.keybinding,
        command: input.keybindingCommand,
      });

      if (isDesktopHost && keybindingRule) {
        return mapAtomCommandResult(
          await upsertKeybinding({
            environmentId,
            input: keybindingRule,
          }),
          () => undefined,
        );
      }
      return updateResult;
    },
    [environmentId, updateProject, upsertKeybinding],
  );
  const saveProjectScript = useCallback(
    async (input: NewProjectScriptInput): Promise<AtomCommandResult<void, unknown>> => {
      if (!activeProject) {
        return AsyncResult.success(undefined);
      }
      const nextId = nextProjectScriptId(
        input.name,
        activeProject.scripts.map((script) => script.id),
      );
      const nextScript: ProjectScript = {
        id: nextId,
        name: input.name,
        command: input.command,
        icon: input.icon,
        runOnWorktreeCreate: input.runOnWorktreeCreate,
      };
      const nextScripts = input.runOnWorktreeCreate
        ? [
            ...activeProject.scripts.map((script) =>
              script.runOnWorktreeCreate ? { ...script, runOnWorktreeCreate: false } : script,
            ),
            nextScript,
          ]
        : [...activeProject.scripts, nextScript];

      return persistProjectScripts({
        projectId: activeProject.id,
        projectCwd: activeProject.workspaceRoot,
        previousScripts: activeProject.scripts,
        nextScripts,
        keybinding: input.keybinding,
        keybindingCommand: commandForProjectScript(nextId),
      });
    },
    [activeProject, persistProjectScripts],
  );
  const updateProjectScript = useCallback(
    async (
      scriptId: string,
      input: NewProjectScriptInput,
    ): Promise<AtomCommandResult<void, unknown>> => {
      if (!activeProject) {
        return AsyncResult.success(undefined);
      }
      const existingScript = activeProject.scripts.find((script) => script.id === scriptId);
      if (!existingScript) {
        return AsyncResult.failure(Cause.fail(new Error("Script not found.")));
      }

      const updatedScript: ProjectScript = {
        ...existingScript,
        name: input.name,
        command: input.command,
        icon: input.icon,
        runOnWorktreeCreate: input.runOnWorktreeCreate,
      };
      const nextScripts = activeProject.scripts.map((script) =>
        script.id === scriptId
          ? updatedScript
          : input.runOnWorktreeCreate
            ? { ...script, runOnWorktreeCreate: false }
            : script,
      );

      return persistProjectScripts({
        projectId: activeProject.id,
        projectCwd: activeProject.workspaceRoot,
        previousScripts: activeProject.scripts,
        nextScripts,
        keybinding: input.keybinding,
        keybindingCommand: commandForProjectScript(scriptId),
      });
    },
    [activeProject, persistProjectScripts],
  );
  const deleteProjectScript = useCallback(
    async (scriptId: string): Promise<AtomCommandResult<void, unknown>> => {
      if (!activeProject) {
        return AsyncResult.success(undefined);
      }
      const nextScripts = activeProject.scripts.filter((script) => script.id !== scriptId);

      const deletedName = activeProject.scripts.find((s) => s.id === scriptId)?.name;

      const result = await persistProjectScripts({
        projectId: activeProject.id,
        projectCwd: activeProject.workspaceRoot,
        previousScripts: activeProject.scripts,
        nextScripts,
        keybinding: null,
        keybindingCommand: commandForProjectScript(scriptId),
      });
      if (result._tag === "Success") {
        toastManager.add({
          type: "success",
          title: `Deleted action "${deletedName ?? "Unknown"}"`,
        });
      } else if (!isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not delete action",
            description: error instanceof Error ? error.message : "An unexpected error occurred.",
          }),
        );
      }
      return result;
    },
    [activeProject, persistProjectScripts],
  );

  const handleRuntimeModeChange = useCallback(
    (mode: RuntimeMode) => {
      if (mode === runtimeMode) return;
      setComposerDraftRuntimeMode(composerDraftTarget, mode);
      if (isLocalDraftThread) {
        setDraftThreadContext(composerDraftTarget, { runtimeMode: mode });
      }
      scheduleComposerFocus();
    },
    [
      isLocalDraftThread,
      runtimeMode,
      scheduleComposerFocus,
      composerDraftTarget,
      setComposerDraftRuntimeMode,
      setDraftThreadContext,
    ],
  );

  const handleInteractionModeChange = useCallback(
    (mode: ProviderInteractionMode) => {
      if (mode === interactionMode) return;
      setComposerDraftInteractionMode(composerDraftTarget, mode);
      if (isLocalDraftThread) {
        setDraftThreadContext(composerDraftTarget, { interactionMode: mode });
      }
      scheduleComposerFocus();
    },
    [
      interactionMode,
      isLocalDraftThread,
      scheduleComposerFocus,
      composerDraftTarget,
      setComposerDraftInteractionMode,
      setDraftThreadContext,
    ],
  );
  const toggleInteractionMode = useCallback(() => {
    handleInteractionModeChange(interactionMode === "plan" ? "default" : "plan");
  }, [handleInteractionModeChange, interactionMode]);
  const dismissPlanSidebarForCurrentTurn = useCallback(() => {
    planSidebarDismissedForTurnRef.current =
      activePlan?.turnId ?? sidebarProposedPlan?.turnId ?? "__dismissed__";
  }, [activePlan?.turnId, sidebarProposedPlan?.turnId]);
  const togglePlanSidebar = useCallback(() => {
    if (!activeThreadRef) return;
    if (planSidebarOpen) {
      dismissPlanSidebarForCurrentTurn();
    } else {
      planSidebarDismissedForTurnRef.current = null;
    }
    useRightPanelStore.getState().toggle(activeThreadRef, "plan");
  }, [activeThreadRef, dismissPlanSidebarForCurrentTurn, planSidebarOpen]);
  const closePlanSidebar = useCallback(() => {
    if (!activeThreadRef) return;
    setMaximizedRightPanelThreadKey(null);
    useRightPanelStore.getState().close(activeThreadRef);
    dismissPlanSidebarForCurrentTurn();
  }, [activeThreadRef, dismissPlanSidebarForCurrentTurn]);
  const createBrowserSurface = useCallback(() => {
    if (!activeThreadRef) return;
    void addBrowserSurface({ threadRef: activeThreadRef, openPreview });
  }, [activeThreadRef, openPreview]);
  const addDiffSurface = useCallback(() => {
    if (!activeThreadRef || !gitRightPanelAvailable) return;
    useRightPanelStore.getState().open(activeThreadRef, "diff");
    onDiffPanelOpen?.();
  }, [activeThreadRef, gitRightPanelAvailable, onDiffPanelOpen]);
  const addSourceControlSurface = useCallback(() => {
    if (!activeThreadRef || !gitRightPanelAvailable) return;
    useRightPanelStore.getState().open(activeThreadRef, "sourceControl");
  }, [activeThreadRef, gitRightPanelAvailable]);
  const addFilesSurface = useCallback(() => {
    if (!activeThreadRef || !activeProject) return;
    useRightPanelStore.getState().open(activeThreadRef, "files");
  }, [activeProject, activeThreadRef]);
  const deviceAvailability = useDevicePreviewsAvailability(activeThreadRef?.environmentId ?? null);
  const addDeviceSurface = useCallback(() => {
    if (!activeThreadRef) return;
    useRightPanelStore.getState().open(activeThreadRef, "device");
  }, [activeThreadRef]);
  // Hidden, not disabled, while the setting is off: Settings is the way in.
  const onAddDevice = deviceAvailability.enabled ? addDeviceSurface : undefined;
  const openFileSurface = useCallback(
    (relativePath: string) => {
      if (!activeThreadRef || !activeProject) return;
      useRightPanelStore.getState().openFile(activeThreadRef, relativePath);
    },
    [activeProject, activeThreadRef],
  );
  const togglePreviewPanel = useCallback(() => {
    if (!activeThreadRef || !isPreviewSupportedInRuntime()) return;
    if (previewPanelOpen) {
      useRightPanelStore.getState().close(activeThreadRef);
      return;
    }
    const activeTabId = activePreviewState.activeTabId;
    if (activeTabId) {
      useRightPanelStore.getState().openBrowser(activeThreadRef, activeTabId);
    } else {
      createBrowserSurface();
    }
  }, [activePreviewState.activeTabId, activeThreadRef, createBrowserSurface, previewPanelOpen]);
  const closePreviewPanel = useCallback(() => {
    if (activeThreadRef) {
      setMaximizedRightPanelThreadKey(null);
      useRightPanelStore.getState().close(activeThreadRef);
    }
  }, [activeThreadRef]);
  const reserveActiveTerminalId = useCallback((): TerminalIdReservation | null => {
    if (!activeThreadRef) return null;
    const centerTerminalIds = selectThreadCenterPanelState(
      useCenterPanelStore.getState().byThreadKey,
      activeThreadRef,
    ).surfaces.flatMap((surface) => (surface.kind === "terminal" ? [surface.terminalId] : []));
    const rightTerminalIds = selectThreadRightPanelState(
      useRightPanelStore.getState().byThreadKey,
      activeThreadRef,
    ).surfaces.flatMap((surface) => (surface.kind === "terminal" ? surface.terminalIds : []));
    return reserveTerminalId(activeThreadRef, [
      ...activeKnownTerminalIds,
      ...centerTerminalIds,
      ...rightTerminalIds,
    ]);
  }, [activeKnownTerminalIds, activeThreadRef]);
  const openReservedRightPanelTerminal = useCallback(
    (reservation: TerminalIdReservation, cwd: string) => {
      if (!activeThreadRef || !activeThreadId || !activeProject) {
        reservation.release();
        return;
      }
      void (async () => {
        await openTerminal({
          environmentId: activeThreadRef.environmentId,
          input: {
            threadId: activeThreadId,
            terminalId: reservation.terminalId,
            cwd,
            ...(activeThreadWorktreePath != null ? { worktreePath: activeThreadWorktreePath } : {}),
            env: projectScriptRuntimeEnv({
              project: { cwd: activeProject.workspaceRoot },
              worktreePath: activeThreadWorktreePath,
            }),
          },
        });
      })().finally(() => {
        reservation.release();
      });
    },
    [activeProject, activeThreadId, activeThreadRef, activeThreadWorktreePath, openTerminal],
  );
  const addTerminalSurface = useCallback(() => {
    if (!activeThreadRef || !activeThreadId || !activeProject || workspaceUnavailable) return;
    const cwd = gitCwd ?? activeProject.workspaceRoot;
    const reservation = reserveActiveTerminalId();
    if (!reservation) return;
    const terminalId = reservation.terminalId;
    useRightPanelStore.getState().openTerminal(activeThreadRef, terminalId);
    setTerminalFocusRequestId((value) => value + 1);
    openReservedRightPanelTerminal(reservation, cwd);
  }, [
    activeProject,
    activeThreadId,
    activeThreadRef,
    gitCwd,
    openReservedRightPanelTerminal,
    reserveActiveTerminalId,
    workspaceUnavailable,
  ]);
  const splitPanelTerminal = useCallback(
    (direction: "horizontal" | "vertical" = "horizontal") => {
      if (
        !activeThreadRef ||
        !activeThreadId ||
        !activeProject ||
        workspaceUnavailable !== null ||
        activeRightPanelSurface?.kind !== "terminal" ||
        activeRightPanelSurface.terminalIds.length >= MAX_TERMINALS_PER_GROUP
      ) {
        return;
      }
      const cwd = gitCwd ?? activeProject.workspaceRoot;
      const reservation = reserveActiveTerminalId();
      if (!reservation) return;
      const terminalId = reservation.terminalId;
      const didSplit = useRightPanelStore
        .getState()
        .splitTerminal(activeThreadRef, activeRightPanelSurface.id, terminalId, direction);
      if (!didSplit) {
        reservation.release();
        return;
      }
      setTerminalFocusRequestId((value) => value + 1);
      openReservedRightPanelTerminal(reservation, cwd);
    },
    [
      activeProject,
      activeRightPanelSurface,
      activeThreadId,
      activeThreadRef,
      gitCwd,
      openReservedRightPanelTerminal,
      reserveActiveTerminalId,
      workspaceUnavailable,
    ],
  );
  const splitPanelTerminalVertical = useCallback(() => {
    splitPanelTerminal("vertical");
  }, [splitPanelTerminal]);
  const activatePanelTerminal = useCallback(
    (terminalId: string) => {
      if (!activeThreadRef || activeRightPanelSurface?.kind !== "terminal") return;
      useRightPanelStore
        .getState()
        .activateTerminal(activeThreadRef, activeRightPanelSurface.id, terminalId);
      setTerminalFocusRequestId((value) => value + 1);
    },
    [activeRightPanelSurface, activeThreadRef],
  );
  const closePanelTerminal = useCallback(
    (terminalId: string) => {
      if (!activeThreadRef || activeRightPanelSurface?.kind !== "terminal") return;
      const closePromise = retireTerminalResource({
        environmentId: activeThreadRef.environmentId,
        threadId: activeThreadRef.threadId,
        terminalId,
      });
      useRightPanelStore
        .getState()
        .closeTerminal(activeThreadRef, activeRightPanelSurface.id, terminalId);
      setTerminalFocusRequestId((value) => value + 1);
      return closePromise;
    },
    [activeRightPanelSurface, activeThreadRef, retireTerminalResource],
  );
  const activateRightPanelSurface = useCallback(
    (surface: RightPanelSurface) => {
      if (!activeThreadRef) return;
      if (surface.kind === "plan") {
        planSidebarDismissedForTurnRef.current = null;
      } else if (planSidebarOpen) {
        dismissPlanSidebarForCurrentTurn();
      }
      useRightPanelStore.getState().activateSurface(activeThreadRef, surface.id);
      if (surface.kind === "preview" && surface.resourceId) {
        setActivePreviewTab(activeThreadRef, surface.resourceId);
      }
      if (surface.kind === "terminal") {
        setTerminalFocusRequestId((value) => value + 1);
      }
      if (surface.kind === "diff" && !diffOpen) {
        onDiffPanelOpen?.();
      }
    },
    [activeThreadRef, diffOpen, dismissPlanSidebarForCurrentTurn, onDiffPanelOpen, planSidebarOpen],
  );
  const toggleRightPanel = useCallback(() => {
    if (!activeThreadRef) return;
    if (effectiveRightPanelOpen) {
      if (planSidebarOpen) {
        closePlanSidebar();
      } else {
        closePreviewPanel();
      }
      return;
    }
    if (hostActivitySurfaceSuppressed) {
      addFilesSurface();
      return;
    }
    useRightPanelStore.getState().toggleVisibility(activeThreadRef);
  }, [
    activeThreadRef,
    addFilesSurface,
    closePlanSidebar,
    closePreviewPanel,
    effectiveRightPanelOpen,
    hostActivitySurfaceSuppressed,
    planSidebarOpen,
  ]);
  const toggleRightPanelMaximized = useCallback(() => {
    if (!canMaximizeRightPanel) return;
    setMaximizedRightPanelThreadKey((threadKey) =>
      threadKey === routeThreadKey ? null : routeThreadKey,
    );
  }, [canMaximizeRightPanel, routeThreadKey]);
  const cleanupRightPanelSurfaces = useCallback(
    (surfaces: readonly RightPanelSurface[]) => {
      if (!activeThreadRef) return;
      if (surfaces.some((surface) => surface.kind === "plan")) {
        dismissPlanSidebarForCurrentTurn();
      }

      for (const surface of surfaces) {
        if (surface.kind === "preview" && surface.resourceId) {
          void closePreviewSession({
            closePreview,
            snapshot: activePreviewState.sessions[surface.resourceId] ?? null,
            tabId: surface.resourceId,
            threadRef: activeThreadRef,
          });
        }
        if (surface.kind === "terminal") {
          for (const terminalId of surface.terminalIds) {
            void retireTerminalResource({
              environmentId: activeThreadRef.environmentId,
              threadId: activeThreadRef.threadId,
              terminalId,
            });
          }
        }
      }
    },
    [
      activeThreadRef,
      activePreviewState.sessions,
      closePreview,
      dismissPlanSidebarForCurrentTurn,
      retireTerminalResource,
    ],
  );
  const syncActivePreviewSurface = useCallback(() => {
    if (!activeThreadRef) return;
    const nextActiveSurface = selectActiveRightPanelSurface(
      useRightPanelStore.getState().byThreadKey,
      activeThreadRef,
    );
    if (nextActiveSurface?.kind === "preview" && nextActiveSurface.resourceId) {
      setActivePreviewTab(activeThreadRef, nextActiveSurface.resourceId);
    }
  }, [activeThreadRef]);
  const closeRightPanelSurface = useCallback(
    (surface: RightPanelSurface) => {
      if (!activeThreadRef) return;
      cleanupRightPanelSurfaces([surface]);
      useRightPanelStore.getState().closeSurface(activeThreadRef, surface.id);
      syncActivePreviewSurface();
    },
    [activeThreadRef, cleanupRightPanelSurfaces, syncActivePreviewSurface],
  );
  const closePanelActivitySurface = useCallback(
    (surface: RightPanelSurface) => {
      if (
        !isPanel ||
        activeActivitySurface === null ||
        surface.kind !== "activity" ||
        surface.id !== activeActivitySurface.id
      ) {
        return;
      }
      closeRightPanelSurface(activeActivitySurface);
    },
    [activeActivitySurface, closeRightPanelSurface, isPanel],
  );
  const ignorePanelActivityMultiSurfaceClose = useCallback(
    (_surface: RightPanelSurface) => undefined,
    [],
  );
  const closeAllPanelActivitySurfaces = useCallback(() => {
    if (!isPanel || activeActivitySurface === null) return;
    closeRightPanelSurface(activeActivitySurface);
  }, [activeActivitySurface, closeRightPanelSurface, isPanel]);
  const closeOtherRightPanelSurfaces = useCallback(
    (surface: RightPanelSurface) => {
      if (!activeThreadRef) return;
      const surfaces = rightPanelState.surfaces.filter((entry) => entry.id !== surface.id);
      cleanupRightPanelSurfaces(surfaces);
      useRightPanelStore.getState().closeOtherSurfaces(activeThreadRef, surface.id);
      syncActivePreviewSurface();
    },
    [
      activeThreadRef,
      cleanupRightPanelSurfaces,
      rightPanelState.surfaces,
      syncActivePreviewSurface,
    ],
  );
  const closeRightPanelSurfacesToRight = useCallback(
    (surface: RightPanelSurface) => {
      if (!activeThreadRef) return;
      const surfaceIndex = rightPanelState.surfaces.findIndex((entry) => entry.id === surface.id);
      if (surfaceIndex < 0) return;
      const surfaces = rightPanelState.surfaces.slice(surfaceIndex + 1);
      cleanupRightPanelSurfaces(surfaces);
      useRightPanelStore.getState().closeSurfacesToRight(activeThreadRef, surface.id);
      syncActivePreviewSurface();
    },
    [
      activeThreadRef,
      cleanupRightPanelSurfaces,
      rightPanelState.surfaces,
      syncActivePreviewSurface,
    ],
  );
  const closeAllRightPanelSurfaces = useCallback(() => {
    if (!activeThreadRef) return;
    cleanupRightPanelSurfaces(rightPanelState.surfaces);
    useRightPanelStore.getState().closeAllSurfaces(activeThreadRef);
  }, [activeThreadRef, cleanupRightPanelSurfaces, rightPanelState.surfaces]);
  const closeDisplayedRightPanelSurface = isPanel
    ? closePanelActivitySurface
    : closeRightPanelSurface;
  const closeOtherDisplayedRightPanelSurfaces = isPanel
    ? ignorePanelActivityMultiSurfaceClose
    : closeOtherRightPanelSurfaces;
  const closeDisplayedRightPanelSurfacesToRight = isPanel
    ? ignorePanelActivityMultiSurfaceClose
    : closeRightPanelSurfacesToRight;
  const closeAllDisplayedRightPanelSurfaces = isPanel
    ? closeAllPanelActivitySurfaces
    : closeAllRightPanelSurfaces;
  const copyRightPanelFilePath = useCallback((relativePath: string) => {
    if (typeof window === "undefined" || !navigator.clipboard?.writeText) {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Failed to copy path",
          description: "Clipboard API unavailable.",
        }),
      );
      return;
    }

    void navigator.clipboard.writeText(relativePath).then(
      () => {
        toastManager.add({
          type: "success",
          title: "Path copied",
          description: relativePath,
        });
      },
      (error) => {
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Failed to copy path",
            description: error instanceof Error ? error.message : "An error occurred.",
          }),
        );
      },
    );
  }, []);
  useEffect(
    () =>
      subscribePreviewAction((action) => {
        if (action === "toggle-panel") togglePreviewPanel();
      }),
    [togglePreviewPanel],
  );
  const persistThreadSettingsForNextTurn = useCallback(
    async (input: {
      threadId: ThreadId;
      createdAt: string;
      modelSelection?: ModelSelection;
      runtimeMode: RuntimeMode;
      interactionMode: ProviderInteractionMode;
    }): Promise<AtomCommandResult<void, unknown>> => {
      if (!serverThread) {
        return AsyncResult.success(undefined);
      }

      let result: AtomCommandResult<void, unknown> = AsyncResult.success(undefined);
      if (
        input.modelSelection !== undefined &&
        (input.modelSelection.model !== serverThread.modelSelection.model ||
          input.modelSelection.instanceId !== serverThread.modelSelection.instanceId ||
          JSON.stringify(input.modelSelection.options ?? null) !==
            JSON.stringify(serverThread.modelSelection.options ?? null))
      ) {
        result = mapAtomCommandResult(
          await updateThreadMetadata({
            environmentId,
            input: {
              threadId: input.threadId,
              modelSelection: input.modelSelection,
            },
          }),
          () => undefined,
        );
        if (result._tag === "Failure") {
          return result;
        }
      }

      if (input.runtimeMode !== serverThread.runtimeMode) {
        result = mapAtomCommandResult(
          await setThreadRuntimeMode({
            environmentId,
            input: {
              threadId: input.threadId,
              runtimeMode: input.runtimeMode,
              createdAt: input.createdAt,
            },
          }),
          () => undefined,
        );
        if (result._tag === "Failure") {
          return result;
        }
      }

      if (input.interactionMode !== serverThread.interactionMode) {
        result = mapAtomCommandResult(
          await setThreadInteractionMode({
            environmentId,
            input: {
              threadId: input.threadId,
              interactionMode: input.interactionMode,
              createdAt: input.createdAt,
            },
          }),
          () => undefined,
        );
      }
      return result;
    },
    [
      environmentId,
      serverThread,
      setThreadInteractionMode,
      setThreadRuntimeMode,
      updateThreadMetadata,
    ],
  );

  // Debounce *showing* the scroll-to-bottom pill so it doesn't flash during
  // thread switches. LegendList fires scroll events with isAtEnd=false while
  // initialScrollAtEnd is settling; hiding is always immediate.
  const scrollPillThreadIdRef = useRef(activeThreadId);
  useLayoutEffect(() => {
    scrollPillThreadIdRef.current = activeThreadId;
  }, [activeThreadId]);
  const showScrollDebouncer = useRef(
    new Debouncer((threadId: ThreadId | null) => setScrollPillThreadId(threadId), {
      wait: 150,
    }),
  );
  const showScrollToBottom = scrollPillThreadId !== null && scrollPillThreadId === activeThreadId;
  const timelineScrollModeRef = useRef<TimelineScrollMode>("following-end");
  const pendingTimelineAnchorRef = useRef<MessageId | null>(null);
  const positionedTimelineAnchorRef = useRef<MessageId | null>(null);
  const settledTimelineAnchorRef = useRef<MessageId | null>(null);
  const activeTimelineAnchorIndexRef = useRef<number | null>(null);
  const anchorUserScrollGenerationRef = useRef(0);
  const liveFollowUserScrollGenerationRef = useRef<number | null>(0);
  const pendingAnchorScrollRestoreRef = useRef<{
    readonly messageId: MessageId;
    readonly offset: number;
    readonly userScrollGeneration: number;
  } | null>(null);
  const anchorScrollRestoreFrameRef = useRef<number | null>(null);
  const cancelTimelineLiveFollowForUserNavigation = useCallback(() => {
    anchorUserScrollGenerationRef.current += 1;
    timelineScrollModeRef.current = "free-scrolling";
    liveFollowUserScrollGenerationRef.current = null;
    pendingTimelineAnchorRef.current = null;
    positionedTimelineAnchorRef.current = null;
    settledTimelineAnchorRef.current = null;
    activeTimelineAnchorIndexRef.current = null;
    pendingAnchorScrollRestoreRef.current = null;
    if (anchorScrollRestoreFrameRef.current !== null) {
      cancelAnimationFrame(anchorScrollRestoreFrameRef.current);
      anchorScrollRestoreFrameRef.current = null;
    }
  }, []);
  const cancelTimelineLiveFollowForUserNavigationRef = useRef(
    cancelTimelineLiveFollowForUserNavigation,
  );
  useEffect(() => {
    cancelTimelineLiveFollowForUserNavigationRef.current =
      cancelTimelineLiveFollowForUserNavigation;
  }, [cancelTimelineLiveFollowForUserNavigation]);
  const getActiveTimelineTurnMetrics = useCallback(
    (list?: LegendListRef | null) => {
      const resolvedList = list ?? legendListRef.current;
      const anchorIndex = activeTimelineAnchorIndexRef.current;
      const state = resolvedList?.getState();
      if (!resolvedList || !state || anchorIndex === null) {
        return null;
      }

      return getAnchoredTurnMetrics({
        state,
        anchorIndex,
        composerOverlayHeight,
        anchorOffset: CHAT_LIST_ANCHOR_OFFSET,
      });
    },
    [composerOverlayHeight],
  );
  const timelineRealContentOverflowsViewport = useCallback(
    (list?: LegendListRef | null) => {
      const resolvedList = list ?? legendListRef.current;
      const state = resolvedList?.getState();
      if (!resolvedList || !state || state.data.length === 0) {
        return false;
      }

      const lastRowIndex = state.data.length - 1;
      const lastRowTop = state.positionAtIndex(lastRowIndex);
      const lastRowHeight = state.sizeAtIndex(lastRowIndex);
      if (
        typeof lastRowTop !== "number" ||
        typeof lastRowHeight !== "number" ||
        !Number.isFinite(lastRowTop) ||
        !Number.isFinite(lastRowHeight)
      ) {
        return false;
      }

      const realContentBottom = lastRowTop + Math.max(1, lastRowHeight);
      const visibleScrollLength = Math.max(
        0,
        (state.scrollLength ?? 0) - composerOverlayHeight - CHAT_LIST_ANCHOR_OFFSET,
      );
      return realContentBottom > visibleScrollLength;
    },
    [composerOverlayHeight],
  );

  // Live-follow stays active after send/thread-open until an actual list scroll
  // gesture opts out.
  const scrollToEnd = useCallback((animated = false) => {
    isAtEndRef.current = true;
    timelineScrollModeRef.current = "following-end";
    liveFollowUserScrollGenerationRef.current = anchorUserScrollGenerationRef.current;
    pendingTimelineAnchorRef.current = null;
    activeTimelineAnchorIndexRef.current = null;
    showScrollDebouncer.current.cancel();
    setScrollPillThreadId(null);
    void legendListRef.current?.scrollToEnd?.({ animated });
  }, []);
  // The timeline remounts per thread (it is keyed by thread id), so bind to
  // each thread's scroll node; without a thread there is no timeline.
  useEffect(() => {
    if (activeThreadId === null) {
      return;
    }
    let removeListeners: (() => void) | null = null;
    const frame = requestAnimationFrame(() => {
      const scrollNode = legendListRef.current?.getScrollableNode();
      if (!scrollNode) {
        return;
      }
      const handleManualNavigation = () => {
        cancelTimelineLiveFollowForUserNavigationRef.current();
      };
      scrollNode.addEventListener("wheel", handleManualNavigation, {
        passive: true,
      });
      scrollNode.addEventListener("touchmove", handleManualNavigation, {
        passive: true,
      });
      scrollNode.addEventListener("pointerdown", handleManualNavigation, {
        passive: true,
      });
      removeListeners = () => {
        scrollNode.removeEventListener("wheel", handleManualNavigation);
        scrollNode.removeEventListener("touchmove", handleManualNavigation);
        scrollNode.removeEventListener("pointerdown", handleManualNavigation);
      };
    });

    return () => {
      cancelAnimationFrame(frame);
      removeListeners?.();
    };
  }, [activeThreadId]);

  const onTimelineAnchorReady = useCallback((messageId: MessageId, anchorIndex: number) => {
    if (pendingTimelineAnchorRef.current === messageId) {
      pendingTimelineAnchorRef.current = null;
    }
    activeTimelineAnchorIndexRef.current = anchorIndex;
    if (positionedTimelineAnchorRef.current === messageId) {
      return;
    }
    positionedTimelineAnchorRef.current = messageId;
    settledTimelineAnchorRef.current = null;
    const positionAnchor = (remainingAttempts: number) => {
      requestAnimationFrame(() => {
        if (positionedTimelineAnchorRef.current !== messageId) {
          return;
        }
        const list = legendListRef.current;
        if (!list) {
          if (remainingAttempts > 0) {
            positionAnchor(remainingAttempts - 1);
          }
          return;
        }
        const scrollNode = list.getScrollableNode();
        let finished = false;
        const finishAnimatedPositioning = () => {
          if (finished) {
            return;
          }
          finished = true;
          window.clearTimeout(fallbackTimer);
          scrollNode.removeEventListener("scrollend", finishAnimatedPositioning);
          if (positionedTimelineAnchorRef.current !== messageId) {
            return;
          }
          const scrollOffset = list.getState().scroll;
          void list.scrollToOffset({ offset: scrollOffset, animated: false });
          settledTimelineAnchorRef.current = messageId;
        };
        const fallbackTimer = window.setTimeout(finishAnimatedPositioning, 750);
        scrollNode.addEventListener("scrollend", finishAnimatedPositioning, { once: true });
        void list.scrollToIndex({
          index: anchorIndex,
          animated: true,
          viewPosition: 0,
          viewOffset: CHAT_LIST_ANCHOR_OFFSET,
        });
      });
    };
    requestAnimationFrame(() => positionAnchor(12));
  }, []);
  const onTimelineAnchorSizeChanged = useCallback((messageId: MessageId) => {
    if (settledTimelineAnchorRef.current !== messageId) {
      return;
    }
    if (liveFollowUserScrollGenerationRef.current === anchorUserScrollGenerationRef.current) {
      return;
    }
    const scrollOffset = legendListRef.current?.getState().scroll;
    if (scrollOffset === undefined) {
      return;
    }
    if (pendingAnchorScrollRestoreRef.current === null) {
      pendingAnchorScrollRestoreRef.current = {
        messageId,
        offset: scrollOffset,
        userScrollGeneration: anchorUserScrollGenerationRef.current,
      };
    }
    if (anchorScrollRestoreFrameRef.current !== null) {
      return;
    }
    anchorScrollRestoreFrameRef.current = requestAnimationFrame(() => {
      anchorScrollRestoreFrameRef.current = null;
      const pending = pendingAnchorScrollRestoreRef.current;
      pendingAnchorScrollRestoreRef.current = null;
      if (
        pending &&
        settledTimelineAnchorRef.current === pending.messageId &&
        pending.userScrollGeneration === anchorUserScrollGenerationRef.current
      ) {
        const list = legendListRef.current;
        const currentScrollOffset = list?.getState().scroll;
        if (
          typeof currentScrollOffset === "number" &&
          Math.abs(currentScrollOffset - pending.offset) <= 2
        ) {
          void list?.scrollToOffset({ offset: pending.offset, animated: false });
        }
      }
    });
  }, []);

  const onIsAtEndChange = useCallback((isAtEnd: boolean) => {
    if (
      !isAtEnd &&
      liveFollowUserScrollGenerationRef.current === anchorUserScrollGenerationRef.current
    ) {
      showScrollDebouncer.current.cancel();
      setScrollPillThreadId(null);
      return;
    }
    if (isAtEndRef.current === isAtEnd) return;
    isAtEndRef.current = isAtEnd;
    if (isAtEnd) {
      timelineScrollModeRef.current = "following-end";
      liveFollowUserScrollGenerationRef.current = anchorUserScrollGenerationRef.current;
      showScrollDebouncer.current.cancel();
      setScrollPillThreadId(null);
    } else {
      timelineScrollModeRef.current = "free-scrolling";
      liveFollowUserScrollGenerationRef.current = null;
      showScrollDebouncer.current.maybeExecute(scrollPillThreadIdRef.current);
    }
  }, []);

  // Follows the end after new timeline entries render; an empty timeline has nothing to follow.
  useEffect(() => {
    if (!activeThread?.id || timelineEntries.length === 0) {
      return;
    }
    if (liveFollowUserScrollGenerationRef.current !== anchorUserScrollGenerationRef.current) {
      return;
    }

    let secondFrame: number | null = null;
    const frame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(() => {
        if (liveFollowUserScrollGenerationRef.current !== anchorUserScrollGenerationRef.current) {
          return;
        }
        if (pendingTimelineAnchorRef.current !== null) {
          return;
        }
        if (
          positionedTimelineAnchorRef.current !== null &&
          settledTimelineAnchorRef.current !== positionedTimelineAnchorRef.current
        ) {
          return;
        }
        const list = legendListRef.current;
        if (!list) {
          return;
        }

        if (timelineScrollModeRef.current === "anchoring-new-turn") {
          const metrics = getActiveTimelineTurnMetrics(list);
          if (!metrics) {
            return;
          }
          if (metrics.scrollDeltaToRevealEnd <= 1) {
            return;
          }

          const nextOffset = list.getState().scroll + metrics.scrollDeltaToRevealEnd;
          void list.scrollToOffset({ offset: nextOffset, animated: false });
          return;
        }

        if (timelineScrollModeRef.current !== "following-end") {
          return;
        }
        if (!timelineRealContentOverflowsViewport(list)) {
          return;
        }

        void list.scrollToEnd?.({ animated: false });
      });
    });

    return () => {
      cancelAnimationFrame(frame);
      if (secondFrame !== null) {
        cancelAnimationFrame(secondFrame);
      }
    };
  }, [
    activeThread?.id,
    timelineEntries,
    getActiveTimelineTurnMetrics,
    timelineRealContentOverflowsViewport,
  ]);

  // Per-thread view state resets while rendering the newly active thread.
  const [viewStateThreadId, setViewStateThreadId] = useState(activeThreadId);
  if (viewStateThreadId !== activeThreadId) {
    setViewStateThreadId(activeThreadId);
    setPullRequestDialogState(null);
    setScrollPillThreadId(null);
    setIsRevertingCheckpoint(false);
  }

  useEffect(() => {
    isAtEndRef.current = true;
    timelineScrollModeRef.current = "following-end";
    liveFollowUserScrollGenerationRef.current = anchorUserScrollGenerationRef.current;
    pendingTimelineAnchorRef.current = null;
    positionedTimelineAnchorRef.current = null;
    settledTimelineAnchorRef.current = null;
    activeTimelineAnchorIndexRef.current = null;
    showScrollDebouncer.current.cancel();
    if (planSidebarOpenOnNextThreadRef.current) {
      planSidebarOpenOnNextThreadRef.current = false;
      if (activeThreadRef) {
        useRightPanelStore.getState().open(activeThreadRef, "plan");
      }
    }
    planSidebarDismissedForTurnRef.current = null;
  }, [activeThreadRef]);

  // Auto-open the plan sidebar when plan/todo steps arrive for the current turn.
  // Don't auto-open for plans carried over from a previous turn (the user can open manually).
  useEffect(() => {
    if (!autoOpenPlanSidebar) return;
    if (!activePlan) return;
    if (planSidebarOpen) return;
    const latestTurnId = activeLatestTurn?.turnId ?? null;
    if (latestTurnId && activePlan.turnId !== latestTurnId) return;
    const turnKey = activePlan.turnId ?? sidebarProposedPlan?.turnId ?? "__dismissed__";
    if (planSidebarDismissedForTurnRef.current === turnKey) return;
    if (activeThreadRef) {
      useRightPanelStore.getState().open(activeThreadRef, "plan");
    }
  }, [
    activePlan,
    activeLatestTurn?.turnId,
    activeThreadRef,
    autoOpenPlanSidebar,
    planSidebarOpen,
    sidebarProposedPlan?.turnId,
  ]);

  useEffect(() => {
    if (!activeThread?.id) return;
    if (activeThread.messages.length === 0) {
      return;
    }
    const serverIds = new Set(activeThread.messages.map((message) => message.id));
    const removedMessages = optimisticUserMessages.filter((message) => serverIds.has(message.id));
    if (removedMessages.length === 0) {
      return;
    }
    const timer = window.setTimeout(() => {
      setOptimisticUserMessages((existing) =>
        existing.filter((message) => !serverIds.has(message.id)),
      );
    }, 0);
    for (const removedMessage of removedMessages) {
      const previewUrls = collectUserMessageBlobPreviewUrls(removedMessage);
      if (previewUrls.length > 0) {
        handoffAttachmentPreviews(removedMessage.id, previewUrls);
        continue;
      }
      revokeUserMessagePreviewUrls(removedMessage);
    }
    return () => {
      window.clearTimeout(timer);
    };
  }, [activeThread?.id, activeThread?.messages, handoffAttachmentPreviews, optimisticUserMessages]);

  // Route-scoped send state resets while rendering the new route.
  const [sendStateRoute, setSendStateRoute] = useState({ draftId, threadId });
  if (sendStateRoute.draftId !== draftId || sendStateRoute.threadId !== threadId) {
    setSendStateRoute({ draftId, threadId });
    setOptimisticUserMessages([]);
    resetLocalDispatch();
    setExpandedImage(null);
  }
  // The previous route's previews are released once the new route commits: a route switch
  // whose render is discarded must leave the still-visible thread's images intact.
  const committedOptimisticRef = useRef({ draftId, threadId, messages: optimisticUserMessages });
  useEffect(() => {
    const previous = committedOptimisticRef.current;
    if (previous.draftId !== draftId || previous.threadId !== threadId) {
      for (const message of previous.messages) {
        revokeUserMessagePreviewUrls(message);
      }
    }
    committedOptimisticRef.current = { draftId, threadId, messages: optimisticUserMessages };
  }, [draftId, optimisticUserMessages, threadId]);

  const closeExpandedImage = useCallback(() => {
    setExpandedImage(null);
  }, []);

  const activeWorktreePath = activeThread?.worktreePath ?? null;
  const derivedEnvMode: DraftThreadEnvMode = resolveEffectiveEnvMode({
    activeWorktreePath,
    hasServerThread: isServerThread,
    draftThreadEnvMode: isLocalDraftThread ? draftThread?.envMode : undefined,
  });
  const canOverrideServerThreadEnvMode = Boolean(
    isServerThread &&
    activeThread &&
    activeThread.messages.length === 0 &&
    activeThread.worktreePath === null &&
    !envLocked,
  );
  const envMode: DraftThreadEnvMode = canOverrideServerThreadEnvMode
    ? (pendingServerThreadEnvMode ?? draftThread?.envMode ?? derivedEnvMode)
    : derivedEnvMode;
  const centerPanelLaunchContext = useMemo(
    () =>
      resolveCenterPanelLaunchContext({
        hasServerThread: isServerThread,
        envMode,
        projectCwd: activeProjectCwd,
        worktreePath: activeThreadWorktreePath,
      }),
    [activeProjectCwd, activeThreadWorktreePath, envMode, isServerThread],
  );
  const centerTerminalLaunchContext = useMemo(
    () =>
      centerPanelLaunchContext && activeProject
        ? {
            ...centerPanelLaunchContext,
            runtimeEnv: projectScriptRuntimeEnv({
              project: { cwd: activeProject.workspaceRoot },
              worktreePath: centerPanelLaunchContext.worktreePath,
            }),
          }
        : null,
    [activeProject, centerPanelLaunchContext],
  );
  // Chat-header "+" panel menu → create a sibling chat panel / open a center
  // terminal, both sharing the live host thread's resolved effective cwd.
  const handleCreateChatPanel = useCallback(
    (entry: ProviderInstanceEntry) => {
      if (!activeThread || !activeThreadRef || !centerPanelLaunchContext || workspaceUnavailable)
        return;
      const configuredDefault = settings.providerSessionDefaults[entry.driverKind];
      const projectDefaultModelSelection = activeProject?.defaultModelSelection;
      const projectSelection =
        projectDefaultModelSelection?.instanceId === entry.instanceId
          ? projectDefaultModelSelection
          : null;
      const resolution = resolveProviderSessionDefault({
        driver: entry.driverKind,
        instanceId: entry.instanceId,
        models: entry.models,
        ...(configuredDefault === undefined ? {} : { configuredDefault }),
        ...(projectSelection === null ? {} : { projectSelection }),
      });
      if (resolution.fallback) {
        console.warn("Provider session default fallback", resolution.fallback);
      }
      void centerPanelActions.createChatPanel({
        hostRef: activeThreadRef,
        projectId: activeThread.projectId,
        worktreePath: centerPanelLaunchContext.worktreePath,
        branch: activeThread.branch ?? null,
        modelSelection: resolution.modelSelection,
        providerLabel: entry.displayName,
      });
    },
    [
      activeProject?.defaultModelSelection,
      activeThread,
      activeThreadRef,
      centerPanelActions,
      centerPanelLaunchContext,
      settings.providerSessionDefaults,
      workspaceUnavailable,
    ],
  );
  const reopenCenterChatPanel = centerPanelActions.reopenChatPanel;
  const handleReopenChatPanel = useCallback(
    (threadId: ThreadId, providerLabel: string) => {
      if (!activeThreadRef) return;
      void reopenCenterChatPanel(activeThreadRef, threadId, providerLabel);
    },
    [activeThreadRef, reopenCenterChatPanel],
  );
  const openCenterTerminal = useCallback(
    async (
      placement: CenterTerminalPlacement,
      options?: OpenTerminalPanelOptions,
      launchOverride?: CenterTerminalLaunch,
    ): Promise<CenterTerminalCreationResult> => {
      if (!activeThreadRef || workspaceUnavailable) {
        const result = {
          status: "rejected" as const,
          reason: workspaceUnavailable ?? "Terminal launch context is unavailable.",
        };
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: "Could not open terminal",
            description: result.reason,
          }),
        );
        return result;
      }
      const reservation = reserveActiveTerminalId();
      if (!reservation) {
        return {
          status: "rejected",
          reason: "Terminal launch context is unavailable.",
        };
      }
      const terminalId = reservation.terminalId;
      const originRouteBinding = centerTerminalRouteBindingRef.current;
      const originWorkspace = centerPanelWorkspaceRef.current;
      const isOriginCurrent = () =>
        centerTerminalRouteBindingRef.current === originRouteBinding &&
        centerPanelWorkspaceRef.current === originWorkspace;
      const terminalTheme = resolveTerminalThemeMode(
        settings.terminalThemePreference,
        (resolvedTheme === "dark" ? "dark" : "light") as TerminalThemeMode,
      );
      const defaultLaunch: CenterTerminalLaunch | null = centerTerminalLaunchContext
        ? {
            cwd: centerTerminalLaunchContext.cwd,
            worktreePath: centerTerminalLaunchContext.worktreePath,
            env: centerTerminalLaunchContext.runtimeEnv,
          }
        : null;
      const baseLaunch = launchOverride ?? defaultLaunch;
      const launchCommand = options?.command ?? baseLaunch?.command;
      const launch = baseLaunch
        ? {
            ...baseLaunch,
            env: mergeTerminalSpawnEnv({
              runtimeEnv: baseLaunch.env,
              commandEnv: launchCommand?.env,
              resolvedTheme: terminalTheme,
              windowsConsoleTheme: usesPersistentWindowsConsoleTheme(launchCommand),
            }),
            ...(options?.label !== undefined ? { label: options.label } : {}),
            ...(options?.command !== undefined ? { command: options.command } : {}),
          }
        : null;
      return (async () => {
        const result = await createCenterTerminal(
          {
            threadRef: activeThreadRef,
            terminalId,
            placement,
            launch,
          },
          {
            validatePlacement: (candidate) =>
              useCenterPanelStore
                .getState()
                .validateTerminalPanelPlacement(activeThreadRef, candidate),
            canSplit: (groupId, direction) =>
              isOriginCurrent() && (originWorkspace?.canSplitGroup(groupId, direction) ?? false),
            openSession: async (input): Promise<CenterTerminalSessionCommandResult> => {
              const dimensions = readTerminalFittedSize(
                activeThreadRef.environmentId,
                input.threadId,
                input.terminalId,
              );
              const openResult = await openTerminal({
                environmentId: activeThreadRef.environmentId,
                input: { ...input, ...dimensions },
              });
              if (openResult._tag === "Success") {
                return { ok: true };
              }
              if (isAtomCommandInterrupted(openResult)) {
                return { ok: false, reason: "Terminal open was interrupted.", interrupted: true };
              }
              const error = squashAtomCommandFailure(openResult);
              return {
                ok: false,
                reason: error instanceof Error ? error.message : "Failed to open terminal.",
              };
            },
            place: (nextTerminalId, candidate, nextOptions) =>
              isOriginCurrent() &&
              useCenterPanelStore
                .getState()
                .placeTerminalPanel(activeThreadRef, nextTerminalId, candidate, nextOptions),
            closeSession: async (input): Promise<CenterTerminalSessionCommandResult> => {
              const closeResult = await closeTerminalMutation({
                environmentId: activeThreadRef.environmentId,
                input,
              });
              if (closeResult._tag === "Success") {
                releaseTerminalUiResources(
                  activeThreadRef.environmentId,
                  input.threadId,
                  input.terminalId ?? terminalId,
                );
                return { ok: true };
              }
              if (isAtomCommandInterrupted(closeResult)) {
                return {
                  ok: false,
                  reason: "Terminal cleanup was interrupted.",
                  interrupted: true,
                };
              }
              const error = squashAtomCommandFailure(closeResult);
              return {
                ok: false,
                reason:
                  error instanceof Error ? error.message : "Failed to close spawned terminal.",
              };
            },
          },
        );
        if (result.status === "opened") {
          if (isOriginCurrent()) {
            setTerminalFocusRequestId((value) => value + 1);
          }
        } else if (!(result.status === "failed" && result.interrupted === true)) {
          toastManager.add(
            stackedThreadToast({
              type: result.status === "rejected" ? "warning" : "error",
              title: "Could not open terminal",
              description: result.reason,
              ...(result.status === "failed" && result.cleanupFailed === true
                ? { timeout: 0 }
                : {}),
            }),
          );
        }
        return result;
      })().finally(() => {
        reservation.release();
      });
    },
    [
      activeThreadRef,
      centerTerminalLaunchContext,
      closeTerminalMutation,
      openTerminal,
      reserveActiveTerminalId,
      resolvedTheme,
      settings.terminalThemePreference,
      workspaceUnavailable,
    ],
  );
  const handleOpenTerminalPanel = useCallback(() => {
    const focusedGroup = selectFocusedCenterPanelGroup(
      selectThreadCenterPanelState(useCenterPanelStore.getState().byThreadKey, activeThreadRef),
    );
    void openCenterTerminal({ type: "tab", groupId: focusedGroup.id });
  }, [activeThreadRef, openCenterTerminal]);
  const handleOpenProviderTerminalPanel = useCallback(
    (action: ProviderTerminalAction) => {
      const focusedGroup = selectFocusedCenterPanelGroup(
        selectThreadCenterPanelState(useCenterPanelStore.getState().byThreadKey, activeThreadRef),
      );
      void openCenterTerminal(
        { type: "tab", groupId: focusedGroup.id },
        { label: action.label, command: action.command },
      );
    },
    [activeThreadRef, openCenterTerminal],
  );
  const runProjectScript = useCallback(
    async (
      script: ProjectScript,
      options?: {
        cwd?: string;
        env?: Record<string, string>;
        worktreePath?: string | null;
        preferNewTerminal?: boolean;
        rememberAsLastInvoked?: boolean;
      },
    ) => {
      if (
        !activeThreadId ||
        !activeThreadRef ||
        !activeProject ||
        !activeThread ||
        readWorkspaceUnavailable()
      )
        return;
      if (options?.rememberAsLastInvoked !== false) {
        setLastInvokedScriptByProjectId((current) => {
          if (current[activeProject.id] === script.id) return current;
          return { ...current, [activeProject.id]: script.id };
        });
      }
      const targetCwd = options?.cwd ?? gitCwd ?? activeProject.workspaceRoot;
      const targetWorktreePath =
        options?.worktreePath !== undefined
          ? options.worktreePath
          : (activeThread.worktreePath ?? null);
      const runtimeEnv = projectScriptRuntimeEnv({
        project: { cwd: activeProject.workspaceRoot },
        worktreePath: targetWorktreePath,
        ...(options?.env ? { extraEnv: options.env } : {}),
      });
      const currentCenterState = selectThreadCenterPanelState(
        useCenterPanelStore.getState().byThreadKey,
        activeThreadRef,
      );
      const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
      const focusedSurface = selectFocusedCenterSurface(currentCenterState);
      const reusableTerminal =
        options?.preferNewTerminal !== true &&
        focusedSurface?.kind === "terminal" &&
        !runningTerminalIds.includes(focusedSurface.terminalId)
          ? focusedSurface
          : null;

      let targetTerminalId: string;
      if (reusableTerminal) {
        const scriptTerminalTheme = resolveTerminalThemeMode(
          settings.terminalThemePreference,
          (resolvedTheme === "dark" ? "dark" : "light") as TerminalThemeMode,
        );
        const scriptSpawnEnv = mergeTerminalSpawnEnv({
          runtimeEnv,
          resolvedTheme: scriptTerminalTheme,
          windowsConsoleTheme: false,
        });
        const dimensions = readTerminalFittedSize(
          environmentId,
          activeThreadId,
          reusableTerminal.terminalId,
        );
        const openResult = await openTerminal({
          environmentId,
          input: {
            threadId: activeThreadId,
            terminalId: reusableTerminal.terminalId,
            ...dimensions,
            cwd: targetCwd,
            worktreePath: targetWorktreePath,
            env: scriptSpawnEnv,
          },
        });
        if (openResult._tag === "Failure") {
          if (!isAtomCommandInterrupted(openResult)) {
            const error = squashAtomCommandFailure(openResult);
            setThreadError(
              activeThreadId,
              error instanceof Error ? error.message : `Failed to run script "${script.name}".`,
            );
          }
          return;
        }
        if (readWorkspaceUnavailable()) return;
        centerPanelActions.activateSurface(activeThreadRef, focusedGroup.id, reusableTerminal.id);
        setTerminalFocusRequestId((value) => value + 1);
        targetTerminalId = reusableTerminal.terminalId;
      } else {
        const creationResult = await openCenterTerminal(
          { type: "tab", groupId: focusedGroup.id },
          { label: script.name },
          {
            cwd: targetCwd,
            worktreePath: targetWorktreePath,
            env: runtimeEnv,
            label: script.name,
          },
        );
        if (creationResult.status !== "opened") {
          if (!(creationResult.status === "failed" && creationResult.interrupted === true)) {
            setThreadError(activeThreadId, creationResult.reason);
          }
          return;
        }
        if (readWorkspaceUnavailable()) return;
        targetTerminalId = creationResult.terminalId;
      }

      if (readWorkspaceUnavailable()) return;
      enqueueTerminalInput({
        environmentId,
        threadId: activeThreadId,
        terminalId: targetTerminalId,
        data: `${script.command}\r`,
        fallbackError: `Failed to run script "${script.name}".`,
        write: (data) =>
          writeTerminal({
            environmentId,
            input: { threadId: activeThreadId, terminalId: targetTerminalId, data },
          }),
        onWriteError: (error) => {
          setThreadError(
            activeThreadId,
            error instanceof Error ? error.message : `Failed to run script "${script.name}".`,
          );
        },
      });
    },
    [
      activeProject,
      activeThread,
      activeThreadId,
      activeThreadRef,
      centerPanelActions,
      environmentId,
      gitCwd,
      openCenterTerminal,
      openTerminal,
      readWorkspaceUnavailable,
      runningTerminalIds,
      setLastInvokedScriptByProjectId,
      setThreadError,
      resolvedTheme,
      settings.terminalThemePreference,
      writeTerminal,
    ],
  );
  const activeThreadBranch =
    canOverrideServerThreadEnvMode && pendingServerThreadBranch !== undefined
      ? pendingServerThreadBranch
      : (activeThread?.branch ?? null);
  const startFromOrigin = isLocalDraftThread
    ? (draftThread?.startFromOrigin ?? false)
    : canOverrideServerThreadEnvMode
      ? (pendingServerThreadStartFromOriginByThreadId[activeThread?.id ?? ""] ??
        settings.newWorktreesStartFromOrigin)
      : false;
  const sendEnvMode = resolveSendEnvMode({
    requestedEnvMode: envMode,
    isGitRepo,
  });

  useEffect(() => {
    const handler = (event: globalThis.KeyboardEvent) => {
      if (!activeThreadId || isCommandPaletteOpen()) {
        return;
      }
      const terminalFocusOwner = getTerminalFocusOwner();
      if (event.defaultPrevented && terminalFocusOwner === null) {
        return;
      }
      const shortcutContext = {
        terminalFocus: terminalFocusOwner !== null,
        terminalOpen: hasTerminalSurface,
        modelPickerOpen: composerRef.current?.isModelPickerOpen() ?? false,
      };

      if (
        !shortcutContext.terminalFocus &&
        !shortcutContext.modelPickerOpen &&
        shouldTypeToFocusComposer(event)
      ) {
        if (composerRef.current?.insertTextAtEnd(event.key)) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
      }

      const command = resolveShortcutCommand(event, keybindings, {
        context: shortcutContext,
      });
      if (!command) return;

      if (command === "terminal.newCenter") {
        event.preventDefault();
        event.stopPropagation();
        const currentCenterState = selectThreadCenterPanelState(
          useCenterPanelStore.getState().byThreadKey,
          activeThreadRef,
        );
        const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
        void openCenterTerminal({ type: "tab", groupId: focusedGroup.id });
        return;
      }

      if (command === "rightPanel.toggle") {
        event.preventDefault();
        event.stopPropagation();
        toggleRightPanel();
        return;
      }

      if (command === "terminal.split") {
        event.preventDefault();
        event.stopPropagation();
        if (terminalFocusOwner === "right-panel") {
          splitPanelTerminal();
          return;
        }
        if (terminalFocusOwner === "center-panel") {
          const currentCenterState = selectThreadCenterPanelState(
            useCenterPanelStore.getState().byThreadKey,
            activeThreadRef,
          );
          const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
          void openCenterTerminal({
            type: "split",
            groupId: focusedGroup.id,
            direction: "right",
          });
        }
        return;
      }

      if (command === "terminal.splitVertical") {
        event.preventDefault();
        event.stopPropagation();
        if (terminalFocusOwner === "right-panel") {
          splitPanelTerminal("vertical");
          return;
        }
        if (terminalFocusOwner === "center-panel") {
          const currentCenterState = selectThreadCenterPanelState(
            useCenterPanelStore.getState().byThreadKey,
            activeThreadRef,
          );
          const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
          void openCenterTerminal({
            type: "split",
            groupId: focusedGroup.id,
            direction: "down",
          });
        }
        return;
      }

      if (command === "terminal.close") {
        event.preventDefault();
        event.stopPropagation();
        if (terminalFocusOwner === "right-panel" && activeRightPanelSurface?.kind === "terminal") {
          closePanelTerminal(activeRightPanelSurface.activeTerminalId);
          return;
        }
        if (terminalFocusOwner === "center-panel") {
          const currentCenterState = selectThreadCenterPanelState(
            useCenterPanelStore.getState().byThreadKey,
            activeThreadRef,
          );
          const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
          const focusedSurface = selectFocusedCenterSurface(currentCenterState);
          if (focusedSurface?.kind === "terminal") {
            closeCenterPanelSurface(focusedGroup.id, focusedSurface);
            setTerminalFocusRequestId((value) => value + 1);
          }
        }
        return;
      }

      if (command === "terminal.new") {
        event.preventDefault();
        event.stopPropagation();
        if (terminalFocusOwner === "right-panel") {
          addTerminalSurface();
          return;
        }
        if (terminalFocusOwner === "center-panel") {
          const currentCenterState = selectThreadCenterPanelState(
            useCenterPanelStore.getState().byThreadKey,
            activeThreadRef,
          );
          const focusedGroup = selectFocusedCenterPanelGroup(currentCenterState);
          void openCenterTerminal({ type: "tab", groupId: focusedGroup.id });
        }
        return;
      }

      if (command === "diff.toggle") {
        event.preventDefault();
        event.stopPropagation();
        onToggleDiff();
        return;
      }

      if (command === "modelPicker.toggle") {
        event.preventDefault();
        event.stopPropagation();
        composerRef.current?.toggleModelPicker();
        return;
      }

      const scriptId = projectScriptIdFromCommand(command);
      if (!scriptId || !activeProject) return;
      const script = activeProject.scripts.find((entry) => entry.id === scriptId);
      if (!script) return;
      event.preventDefault();
      event.stopPropagation();
      void runProjectScript(script);
    };
    // Panel variant: global keybindings are host-owned; N panels must not each
    // register a document-level keydown handler (commands would fire N times).
    if (isPanel) return;
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [
    isPanel,
    activeProject,
    activeRightPanelSurface,
    activeThreadRef,
    addTerminalSurface,
    activeThreadId,
    closeCenterPanelSurface,
    closePanelTerminal,
    hasTerminalSurface,
    openCenterTerminal,
    runProjectScript,
    splitPanelTerminal,
    keybindings,
    onToggleDiff,
    toggleRightPanel,
    composerRef,
  ]);

  const onRevertToTurnCount = useCallback(
    async (turnCount: number) => {
      const localApi = readLocalApi();
      if (!localApi || !activeThread || isRevertingCheckpoint) return;

      if (activeEnvironmentUnavailable && activeEnvironmentUnavailableLabel) {
        setThreadError(
          activeThread.id,
          `Reconnect ${activeEnvironmentUnavailableLabel} before reverting checkpoints.`,
        );
        return;
      }
      if (phase === "running" || isSendBusy || isConnecting) {
        setThreadError(activeThread.id, "Interrupt the current turn before reverting checkpoints.");
        return;
      }
      const confirmed = await localApi.dialogs.confirm(
        buildRevertCheckpointConfirmation({
          turnCount,
          provider: activeProviderStatus,
        }),
      );
      if (!confirmed) {
        return;
      }

      setIsRevertingCheckpoint(true);
      setThreadError(activeThread.id, null);
      const result = await revertThreadCheckpoint({
        environmentId,
        input: {
          threadId: activeThread.id,
          turnCount,
        },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        setThreadError(
          activeThread.id,
          error instanceof Error ? error.message : "Failed to revert thread state.",
        );
      }
      setIsRevertingCheckpoint(false);
    },
    [
      activeThread,
      activeEnvironmentUnavailable,
      activeEnvironmentUnavailableLabel,
      activeProviderStatus,
      environmentId,
      isConnecting,
      isRevertingCheckpoint,
      isSendBusy,
      phase,
      revertThreadCheckpoint,
      setThreadError,
    ],
  );

  const onResolveTurnDelivery = useCallback(
    async (messageId: MessageId, action: TurnDeliveryResolutionAction) => {
      if (!activeThread || resolvingTurnDeliveryMessageId === messageId) return;
      const delivery = activeThread.messages.find((message) => message.id === messageId)?.delivery;
      if (!delivery || (delivery.state !== "uncertain" && delivery.state !== "failed")) return;

      if (action === "retry" && delivery.state === "uncertain") {
        const localApi = readLocalApi();
        if (!localApi) return;
        const provider = deliveryProviderLabel(delivery, instanceLabels);
        const confirmed = await localApi.dialogs.confirm(
          [
            "Retry this message?",
            `${provider} may receive a duplicate if it received the original message.`,
            "Only retry if sending the message twice is safe.",
          ].join("\n"),
        );
        if (!confirmed) return;
      }

      setResolvingTurnDeliveryMessageId(messageId);
      setThreadError(activeThread.id, null);
      const result = await resolveTurnDelivery({
        environmentId,
        input: { threadId: activeThread.id, messageId, action },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        setThreadError(
          activeThread.id,
          error instanceof Error ? error.message : "Failed to resolve message delivery.",
        );
      }
      setResolvingTurnDeliveryMessageId((current) => (current === messageId ? null : current));
    },
    [
      activeThread,
      environmentId,
      resolveTurnDelivery,
      resolvingTurnDeliveryMessageId,
      instanceLabels,
      setResolvingTurnDeliveryMessageId,
      setThreadError,
    ],
  );

  const restoreQueuedMessageToComposer = useCallback(
    (message: ChatMessage, cachedAttachments?: ReadonlyArray<ComposerAttachment>) => {
      const store = useComposerDraftStore.getState();
      const storedDraft = store.getComposerDraft(composerDraftTarget);
      const isCurrentComposer = currentQueueThreadKeyRef.current === routeThreadKey;
      const draft = {
        prompt: isCurrentComposer ? promptRef.current : (storedDraft?.prompt ?? ""),
        attachments: storedDraft?.attachments ?? [],
      };
      const restored = mergeQueuedMessageIntoDraft(
        draft,
        { ...message, cachedAttachments },
        {
          maxAttachments: PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
        },
      );
      const attachmentsToAdd = restored.draft.attachments
        .slice(draft.attachments.length)
        .map(cloneComposerAttachmentForRetry);
      store.setPrompt(composerDraftTarget, restored.draft.prompt);
      store.addAttachments(composerDraftTarget, attachmentsToAdd, {
        maxAttachments: PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
      });
      if (isCurrentComposer) {
        promptRef.current = restored.draft.prompt;
        composerAttachmentsRef.current =
          store.getComposerDraft(composerDraftTarget)?.attachments ?? [];
        resetRestoredComposerCursor(composerRef, restored.draft.prompt);
      }
      if (restored.droppedAttachmentCount > 0) {
        const count = restored.droppedAttachmentCount;
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: `${count} ${count === 1 ? "attachment" : "attachments"} not restored`,
            description:
              "The composer attachment limit was reached. Reattach the remaining files after sending this draft.",
          }),
        );
      }
      if (restored.unrestoredAttachmentCount > 0) {
        const count = restored.unrestoredAttachmentCount;
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: `${count} ${count === 1 ? "attachment" : "attachments"} not restored`,
            description:
              "These files were queued in another client or before a reload. Your text was restored; attach the files again.",
          }),
        );
      }
    },
    [composerDraftTarget, composerRef, routeThreadKey],
  );

  const setQueuedMessageError = useCallback(
    (messageId: MessageId, error: string | null) => {
      const key = `${routeThreadKey}:${messageId}`;
      setQueuedMessageErrors((current) => {
        const next = { ...current };
        if (error === null) delete next[key];
        else next[key] = error;
        return next;
      });
    },
    [routeThreadKey],
  );
  const queuedCancellationsRef = useRef(new Map<MessageId, Promise<void>>());
  const onCancelQueuedMessage = useCallback(
    async (messageId: MessageId) => {
      const existing = queuedCancellationsRef.current.get(messageId);
      if (existing) return existing;
      const message = queuedMessages.find((message) => message.id === messageId);
      if (
        !activeThread ||
        message?.delivery?.state !== "queued" ||
        queuedActionsInFlightRef.current.has(messageId)
      )
        return;
      const cancel = async () => {
        queuedActionsInFlightRef.current.add(messageId);
        setResolvingQueuedMessageId(messageId);
        setQueuedMessageError(messageId, null);
        // Retain Files across the withdrawal snapshot arriving before the command receipt.
        const cached = queuedMessageCache.take(messageId);
        try {
          const result = await resolveTurnDelivery({
            environmentId,
            input: { threadId: activeThread.id, messageId, action: "cancel" },
          });
          if (result._tag === "Failure") throw squashAtomCommandFailure(result);
          restoreQueuedMessageToComposer(message, cached?.attachments);
        } catch (error) {
          if (cached) queuedMessageCache.remember(messageId, cached);
          setQueuedMessageError(
            messageId,
            `Could not cancel. ${error instanceof Error ? error.message : "The request failed."} Try again.`,
          );
        } finally {
          queuedActionsInFlightRef.current.delete(messageId);
          setResolvingQueuedMessageId((current) => (current === messageId ? null : current));
        }
      };
      const pending = cancel();
      queuedCancellationsRef.current.set(messageId, pending);
      try {
        await pending;
      } finally {
        queuedCancellationsRef.current.delete(messageId);
      }
    },
    [
      activeThread,
      environmentId,
      queuedMessages,
      resolveTurnDelivery,
      restoreQueuedMessageToComposer,
      setQueuedMessageError,
    ],
  );

  const runQueuedHeadAction = useCallback(
    async (messageId: MessageId, action: "steer" | "send-now") => {
      if (
        !activeThread ||
        queuedMessages[0]?.id !== messageId ||
        stoppingQueueRef.current ||
        queuedActionsInFlightRef.current.has(messageId)
      )
        return;
      const status = queuedStatuses[0];
      if (action === "steer" ? !status?.canSteer : !status?.canSendNow) return;
      queuedActionsInFlightRef.current.add(messageId);
      setResolvingQueuedMessageId(messageId);
      setQueuedMessageError(messageId, null);
      try {
        const command = action === "steer" ? steerThreadTurn : promoteThreadTurn;
        const result = await command({
          environmentId,
          input: { threadId: activeThread.id, messageId },
        });
        if (result._tag === "Failure") throw squashAtomCommandFailure(result);
      } catch (error) {
        setQueuedMessageError(
          messageId,
          `Could not ${action === "steer" ? "steer" : "send"} this message. ${error instanceof Error ? error.message : "The request failed."} Try again.`,
        );
      } finally {
        queuedActionsInFlightRef.current.delete(messageId);
        setResolvingQueuedMessageId((current) => (current === messageId ? null : current));
      }
    },
    [
      activeThread,
      environmentId,
      promoteThreadTurn,
      queuedMessages,
      queuedStatuses,
      setQueuedMessageError,
      steerThreadTurn,
    ],
  );
  const onSteerQueuedMessage = useCallback(
    (messageId: MessageId) => runQueuedHeadAction(messageId, "steer"),
    [runQueuedHeadAction],
  );
  const onSendNowQueuedMessage = useCallback(
    (messageId: MessageId) => runQueuedHeadAction(messageId, "send-now"),
    [runQueuedHeadAction],
  );
  const onSteerHeadQueuedMessage = useCallback(async () => {
    const head = queuedMessages[0];
    if (head) await onSteerQueuedMessage(head.id);
  }, [onSteerQueuedMessage, queuedMessages]);

  const onSend = async (e?: { preventDefault: () => void }) => {
    e?.preventDefault();
    const enqueue = shouldEnqueueOnSend({
      phase,
      sessionStatus: activeThread?.session?.status ?? null,
      hasPendingDelivery: canCancelPendingSend,
    });
    if (
      !activeThread ||
      (isSendBusy && !enqueue) ||
      isConnecting ||
      activeEnvironmentUnavailable ||
      workspaceUnavailable !== null ||
      providerBinding.conflict !== null ||
      stoppingQueueRef.current ||
      sendInFlightRef.current
    )
      return;
    const sendLease = attachmentAdmissionOwner.claim(routeThreadRef);
    if (sendLease === null) return;
    try {
      let attachmentAuthority: AttachmentAdmissionAuthority | undefined;
      const authorizeAttempt = async () => {
        const result = await readAttachmentAdmissionAuthority({ environmentId, input: undefined });
        if (result._tag === "Failure") {
          setThreadError(
            activeThread.id,
            "Reconnect to the original environment before confirming this send. Your draft was kept.",
          );
          return false;
        }
        if (result.value === null) {
          if (stagedAttachmentAttempts.get(routeThreadRef) === undefined) return true;
          setThreadError(
            activeThread.id,
            "The original server cannot identify its data store. Update it before confirming this send; your draft was kept.",
          );
          return false;
        }
        if (!attachmentAdmissionOwner.authorize(routeThreadRef, sendLease, result.value)) {
          setThreadError(
            activeThread.id,
            "An earlier send belongs to another host or data store. Reconnect to the original environment before sending another copy.",
          );
          return false;
        }
        attachmentAuthority = result.value;
        return true;
      };
      if (stagedAttachmentAttempts.get(routeThreadRef) !== undefined && !(await authorizeAttempt()))
        return;
      const ambiguous = stagedAttachmentAttempts.get(routeThreadRef)?.ambiguousAdmission;
      if (ambiguous) {
        sendInFlightRef.current = true;
        const keeperController = new AbortController();
        const retained = stagedAttachmentAttempts.get(routeThreadRef)!;
        const replayMessageId = ambiguous.input.message.messageId;
        attachmentSendControllers.current.set(replayMessageId, {
          threadKey: routeThreadKey,
          controller: keeperController,
          restore: () => {},
          phase: "admitting",
        });
        const keeper = keepStagedAttachmentsAlive(
          { environmentId, input: retained.attachments },
          { signal: keeperController.signal },
        );
        try {
          // Replay the original receipt hash before any get/restage: acceptance consumes IDs.
          const result = await startThreadTurn({
            environmentId,
            input: { ...ambiguous.input, attachmentAuthority: attachmentAuthority! },
          });
          if (result._tag === "Success") {
            stagedAttachmentAttempts.delete(routeThreadRef, sendLease);
            if (currentQueueThreadKeyRef.current === routeThreadKey) {
              const currentDraft = useComposerDraftStore
                .getState()
                .getComposerDraft(composerDraftTarget);
              if (
                currentDraft === ambiguous.restoredDraft &&
                promptRef.current === (currentDraft?.prompt ?? "")
              ) {
                promptRef.current = "";
                clearComposerDraftContent(composerDraftTarget);
                composerAttachmentsRef.current = [];
                composerTerminalContextsRef.current = [];
                composerElementContextsRef.current = [];
                composerRef.current?.resetCursorState();
              } else {
                toastManager.add(
                  stackedThreadToast({
                    type: "info",
                    title: "Earlier send confirmed",
                    description:
                      "Your current draft was kept. Review it before sending another message.",
                  }),
                );
              }
              setThreadError(activeThread.id, null);
            }
          } else {
            const error = squashAtomCommandFailure(result);
            if (isUploadError(error) && error.reason === "not_found") {
              stagedAttachmentAttempts.delete(routeThreadRef, sendLease);
              void releaseStagedAttachments({ environmentId, input: retained.attachments });
            }
            if (currentQueueThreadKeyRef.current === routeThreadKey)
              setThreadError(
                activeThread.id,
                isUploadError(error) && error.reason === "not_found"
                  ? "The earlier send was not accepted and its upload is no longer available. Your draft is unchanged; send it again."
                  : "Could not confirm the earlier send. Your draft is unchanged; reconnect and send again to check it before uploading another copy.",
              );
          }
        } finally {
          keeperController.abort();
          await keeper;
          attachmentSendControllers.current.delete(replayMessageId);
          sendInFlightRef.current = false;
        }
        return;
      }
      if (activePendingProgress) {
        onAdvanceActivePendingUserInput();
        return;
      }
      const sendCtx = composerRef.current?.getSendContext();
      if (!sendCtx) return;
      const {
        attachments: composerAttachments,
        terminalContexts: composerTerminalContexts,
        elementContexts: composerElementContexts,
        previewAnnotations: composerPreviewAnnotations,
        reviewComments: composerReviewComments,
        selectedProvider: ctxSelectedProvider,
        selectedModel: ctxSelectedModel,
        selectedProviderModels: ctxSelectedProviderModels,
        selectedPromptEffort: ctxSelectedPromptEffort,
        selectedModelSelection: ctxSelectedModelSelection,
      } = sendCtx;
      const promptForSend = canonicalizeLegacyComposerFileReferences(promptRef.current);
      const {
        trimmedPrompt: trimmed,
        sendableTerminalContexts: sendableComposerTerminalContexts,
        expiredTerminalContextCount,
        hasSendableContent,
      } = deriveComposerSendState({
        prompt: promptForSend,
        imageCount: composerAttachments.length,
        terminalContexts: composerTerminalContexts,
        elementContextCount:
          composerElementContexts.length +
          composerPreviewAnnotations.length +
          composerReviewComments.length,
      });
      const standaloneColonAction = parseStandaloneComposerBiBCodeAction(trimmed);
      if (standaloneColonAction === "plan" || standaloneColonAction === "default") {
        handleInteractionModeChange(standaloneColonAction);
        promptRef.current = "";
        discardComposerDraftContent(composerDraftTarget);
        composerRef.current?.resetCursorState();
        return;
      }
      if (showPlanFollowUpPrompt && activeProposedPlan) {
        const followUp = resolvePlanFollowUpSubmission({
          draftText: trimmed,
          planMarkdown: activeProposedPlan.planMarkdown,
        });
        promptRef.current = "";
        discardComposerDraftContent(composerDraftTarget);
        composerRef.current?.resetCursorState();
        await onSubmitPlanFollowUp({
          text: followUp.text,
          interactionMode: followUp.interactionMode,
        });
        return;
      }
      if (!hasSendableContent) {
        if (expiredTerminalContextCount > 0) {
          const toastCopy = buildExpiredTerminalContextToastCopy(
            expiredTerminalContextCount,
            "empty",
          );
          toastManager.add(
            stackedThreadToast({
              type: "warning",
              title: toastCopy.title,
              description: toastCopy.description,
            }),
          );
        }
        return;
      }
      if (!activeProject) return;
      if (
        composerAttachments.length > 0 &&
        attachmentAuthority === undefined &&
        !(await authorizeAttempt())
      )
        return;
      if (currentQueueThreadKeyRef.current !== routeThreadKey) return;
      const threadIdForSend = activeThread.id;
      const isFirstMessage = !isServerThread || activeThread.messages.length === 0;
      const baseBranchForWorktree =
        isFirstMessage && sendEnvMode === "worktree" && !activeThread.worktreePath
          ? activeThreadBranch
          : null;

      // In worktree mode, require an explicit base branch so we don't silently
      // fall back to local execution when branch selection is missing.
      const shouldCreateWorktree =
        isFirstMessage && sendEnvMode === "worktree" && !activeThread.worktreePath;
      if (shouldCreateWorktree && !activeThreadBranch) {
        setThreadError(
          threadIdForSend,
          "Select a base branch before sending in New worktree mode.",
        );
        return;
      }

      const messageIdForSend = newMessageId();
      sendInFlightRef.current = true;
      if (!enqueue && composerAttachments.length === 0)
        beginLocalDispatch({
          preparingWorktree: Boolean(baseBranchForWorktree),
          threadId: threadIdForSend,
          messageId: messageIdForSend,
        });

      const composerAttachmentsSnapshot = [...composerAttachments];
      const composerTerminalContextsSnapshot = [...sendableComposerTerminalContexts];
      const composerElementContextsSnapshot = [...composerElementContexts];
      const composerPreviewAnnotationsSnapshot = [...composerPreviewAnnotations];
      const composerReviewCommentsSnapshot: ReviewCommentContext[] = [...composerReviewComments];
      const messageTextWithContexts = appendElementContextsToPrompt(
        appendTerminalContextsToPrompt(promptForSend, composerTerminalContextsSnapshot),
        composerElementContextsSnapshot,
      );
      const messageTextWithPreviewAnnotations = composerPreviewAnnotationsSnapshot.reduce(
        (text, annotation) => appendPreviewAnnotationPrompt(text, annotation),
        messageTextWithContexts,
      );
      const messageTextForSend = appendReviewCommentsToPrompt(
        messageTextWithPreviewAnnotations,
        composerReviewCommentsSnapshot,
      );
      const messageCreatedAt = new Date().toISOString();
      const outgoingMessageText = formatOutgoingPrompt({
        provider: ctxSelectedProvider,
        model: ctxSelectedModel,
        models: ctxSelectedProviderModels,
        effort: ctxSelectedPromptEffort,
        text: messageTextForSend || ATTACHMENT_ONLY_BOOTSTRAP_PROMPT,
      });
      const uploadController = new AbortController();
      let uploadFileName = composerAttachmentsSnapshot[0]?.name ?? "attachment";
      let outgoingRestored = false;
      const restoreOutgoing = () => {
        if (outgoingRestored) return;
        outgoingRestored = true;
        const store = useComposerDraftStore.getState();
        const current = store.getComposerDraft(composerDraftTarget);
        const isCurrent = currentQueueThreadKeyRef.current === routeThreadKey;
        const restored = mergeInterruptedComposerContent(
          {
            prompt: isCurrent ? promptRef.current : (current?.prompt ?? ""),
            attachments: current?.attachments ?? [],
            terminalContexts: current?.terminalContexts ?? [],
            elementContexts: current?.elementContexts ?? [],
            previewAnnotations: current?.previewAnnotations ?? [],
            reviewComments: current?.reviewComments ?? [],
          },
          {
            prompt: promptForSend,
            attachments: composerAttachmentsSnapshot.map((attachment) =>
              current?.attachments.some((existing) => existing.id === attachment.id)
                ? attachment
                : cloneComposerAttachmentForRetry(attachment),
            ),
            terminalContexts: composerTerminalContextsSnapshot,
            elementContexts: composerElementContextsSnapshot,
            previewAnnotations: composerPreviewAnnotationsSnapshot,
            reviewComments: composerReviewCommentsSnapshot,
          },
        );
        store.setPrompt(composerDraftTarget, restored.prompt);
        store.addAttachments(composerDraftTarget, restored.attachments);
        store.setTerminalContexts(composerDraftTarget, restored.terminalContexts);
        store.setElementContexts(composerDraftTarget, restored.elementContexts);
        store.setPreviewAnnotations(composerDraftTarget, restored.previewAnnotations);
        store.setReviewComments(composerDraftTarget, restored.reviewComments);
        if (isCurrent) {
          const restoredDraft = store.getComposerDraft(composerDraftTarget);
          promptRef.current = restoredDraft?.prompt ?? restored.prompt;
          composerAttachmentsRef.current = restoredDraft?.attachments ?? [];
          composerTerminalContextsRef.current = restored.terminalContexts;
          composerElementContextsRef.current = restored.elementContexts;
          resetRestoredComposerCursor(composerRef, promptRef.current);
          setOptimisticUserMessages((existing) => {
            for (const message of existing)
              if (message.id === messageIdForSend) revokeUserMessagePreviewUrls(message);
            return existing.filter((message) => message.id !== messageIdForSend);
          });
        }
        setAttachmentUploads((existing) => {
          if (!(messageIdForSend in existing)) return existing;
          const next = { ...existing };
          delete next[messageIdForSend];
          return next;
        });
      };
      attachmentSendControllers.current.set(messageIdForSend, {
        threadKey: routeThreadKey,
        controller: uploadController,
        restore: restoreOutgoing,
        phase: "staging",
      });
      const oldAttempt = stagedAttachmentAttempts.get(routeThreadRef);
      const reusable = reusableStagedAttachmentAttempt(
        oldAttempt,
        routeThreadKey,
        composerAttachmentsSnapshot,
        Date.now(),
      );
      if (oldAttempt && !reusable) {
        stagedAttachmentAttempts.delete(routeThreadRef, sendLease);
        void releaseStagedAttachments({ environmentId, input: oldAttempt.attachments });
      }
      const turnAttachmentsPromise = prepareTurnAttachments({
        sources: composerAttachmentsSnapshot,
        stage: async () => {
          if (composerAttachmentsSnapshot.length === 0)
            return AsyncResult.success({ _tag: "inline" } as const);
          const result = await stageAttachments(
            {
              environmentId,
              input: {
                attachments: composerAttachmentsSnapshot,
                ...(reusable ? { reusable } : {}),
                onProgress: (progress) => {
                  uploadFileName = progress.fileName;
                  if (
                    uploadController.signal.aborted ||
                    currentQueueThreadKeyRef.current !== routeThreadKey ||
                    !attachmentSendControllers.current.has(messageIdForSend)
                  )
                    return;
                  setAttachmentUploads((existing) => ({
                    ...existing,
                    [messageIdForSend]: {
                      ...progress,
                      attachmentCount: composerAttachmentsSnapshot.length,
                      threadKey: routeThreadKey,
                    },
                  }));
                },
              },
            },
            { signal: uploadController.signal },
          );
          if (
            result._tag === "Success" &&
            result.value._tag === "staged" &&
            !uploadController.signal.aborted &&
            currentQueueThreadKeyRef.current === routeThreadKey
          ) {
            stagedAttachmentAttempts.set(routeThreadRef, sendLease, {
              threadKey: routeThreadKey,
              createdAt: Date.now(),
              sources: composerAttachmentsSnapshot,
              attachments: result.value.attachments,
            });
          }
          return result;
        },
      });
      const optimisticAttachments = composerAttachmentsSnapshot.map((attachment) =>
        attachment.type === "image"
          ? {
              type: "image" as const,
              id: attachment.id,
              name: attachment.name,
              mimeType: attachment.mimeType,
              sizeBytes: attachment.sizeBytes,
              previewUrl: attachment.previewUrl,
            }
          : {
              type: "file" as const,
              id: attachment.id,
              name: attachment.name,
              mimeType: attachment.mimeType,
              sizeBytes: attachment.sizeBytes,
            },
      );
      if (enqueue) {
        queuedMessageCache.remember(messageIdForSend, { attachments: composerAttachmentsSnapshot });
        isAtEndRef.current = true;
        timelineScrollModeRef.current = "following-end";
        void legendListRef.current?.scrollToEnd?.({ animated: false });
      }
      if (!enqueue || composerAttachmentsSnapshot.length > 0) {
        // Sending always returns to the live edge. The new row becomes the
        // anchored end-space target so it lands near the top while the response
        // streams into the reserved space below it.
        isAtEndRef.current = true;
        timelineScrollModeRef.current = "anchoring-new-turn";
        liveFollowUserScrollGenerationRef.current = anchorUserScrollGenerationRef.current;
        pendingTimelineAnchorRef.current = messageIdForSend;
        activeTimelineAnchorIndexRef.current = null;
        showScrollDebouncer.current.cancel();
        setScrollPillThreadId(null);
        setTimelineAnchor({
          threadKey: scopedThreadKey(scopeThreadRef(activeThread.environmentId, threadIdForSend)),
          messageId: messageIdForSend,
        });
        setOptimisticUserMessages((existing) => [
          ...existing,
          {
            id: messageIdForSend,
            role: "user",
            text: outgoingMessageText,
            ...(optimisticAttachments.length > 0 ? { attachments: optimisticAttachments } : {}),
            turnId: null,
            createdAt: messageCreatedAt,
            updatedAt: messageCreatedAt,
            streaming: false,
          },
        ]);
      }
      setThreadError(threadIdForSend, null);
      if (expiredTerminalContextCount > 0) {
        const toastCopy = buildExpiredTerminalContextToastCopy(
          expiredTerminalContextCount,
          "omitted",
        );
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: toastCopy.title,
            description: toastCopy.description,
          }),
        );
      }
      promptRef.current = "";
      clearComposerDraftContent(composerDraftTarget);
      composerRef.current?.resetCursorState();

      let firstComposerAttachment: ComposerAttachment | null = null;
      if (composerAttachmentsSnapshot.length > 0) {
        const firstAttachment = composerAttachmentsSnapshot[0];
        if (firstAttachment) {
          firstComposerAttachment = firstAttachment;
        }
      }
      let titleSeed = trimmed;
      if (!titleSeed) {
        if (firstComposerAttachment) {
          titleSeed = `${firstComposerAttachment.type === "image" ? "Image" : "File"}: ${firstComposerAttachment.name}`;
        } else if (composerTerminalContextsSnapshot.length > 0) {
          titleSeed = formatTerminalContextLabel(composerTerminalContextsSnapshot[0]!);
        } else if (composerElementContextsSnapshot.length > 0) {
          titleSeed = formatElementContextLabel(composerElementContextsSnapshot[0]!);
        } else {
          titleSeed = "New thread";
        }
      }
      const title = truncate(titleSeed);
      const threadCreateModelSelection = ctxSelectedModelSelection;

      let failure: AtomCommandResult<unknown, unknown> | null = null;
      // Auto-title from first message
      if (isFirstMessage && isServerThread) {
        const titleResult = await updateThreadMetadata({
          environmentId,
          input: {
            threadId: threadIdForSend,
            title,
          },
        });
        if (titleResult._tag === "Failure") {
          failure = titleResult;
        }
      }

      if (failure === null && isServerThread) {
        const settingsResult = await persistThreadSettingsForNextTurn({
          threadId: threadIdForSend,
          createdAt: messageCreatedAt,
          ...(ctxSelectedModel ? { modelSelection: ctxSelectedModelSelection } : {}),
          runtimeMode,
          interactionMode,
        });
        if (settingsResult._tag === "Failure") {
          failure = settingsResult;
        }
      }

      const cancelledBeforeAdmission = uploadController.signal.aborted;
      if (failure !== null) uploadController.abort();
      const preparedAttachments = await turnAttachmentsPromise;
      const turnAttachmentsResult = uploadController.signal.aborted
        ? AsyncResult.failure(Cause.interrupt())
        : preparedAttachments;
      const stagingFailed = preparedAttachments._tag === "Failure";
      if (cancelledBeforeAdmission) failure = AsyncResult.failure(Cause.interrupt());
      if (failure === null && turnAttachmentsResult._tag === "Failure") {
        failure = turnAttachmentsResult;
      }

      let turnStartSucceeded = false;
      let turnStartAttempted = false;
      if (failure === null && turnAttachmentsResult._tag === "Success") {
        const bootstrap =
          isLocalDraftThread || baseBranchForWorktree
            ? {
                ...(isLocalDraftThread
                  ? {
                      createThread: {
                        projectId: activeProject.id,
                        title,
                        modelSelection: threadCreateModelSelection,
                        runtimeMode,
                        interactionMode,
                        branch: activeThreadBranch,
                        worktreePath: activeThread.worktreePath,
                        createdAt: activeThread.createdAt,
                      },
                    }
                  : {}),
                ...(baseBranchForWorktree
                  ? {
                      prepareWorktree: {
                        projectCwd: activeProject.workspaceRoot,
                        baseBranch: baseBranchForWorktree,
                        branch: buildTemporaryWorktreeBranchName(randomHex),
                        ...(startFromOrigin ? { startFromOrigin: true } : {}),
                      },
                      runSetupScript: true,
                    }
                  : {}),
              }
            : undefined;
        if (!enqueue)
          beginLocalDispatch({
            preparingWorktree: false,
            threadId: threadIdForSend,
            messageId: messageIdForSend,
          });
        const legacyDraftFallback = fallbackDraftResolution?.fallback ?? null;
        const warningKey = routeThreadKey;
        if (
          isLocalDraftThread &&
          legacyDraftMissingStoredModelSelection &&
          fallbackDraftResolution !== null &&
          legacyDraftFallback &&
          ctxSelectedModelSelection.instanceId ===
            fallbackDraftResolution.modelSelection.instanceId &&
          ctxSelectedModelSelection.model === fallbackDraftResolution.modelSelection.model &&
          !warnedLegacyDraftFallbacksRef.current.has(warningKey)
        ) {
          warnedLegacyDraftFallbacksRef.current.add(warningKey);
          console.warn("Provider session default fallback", legacyDraftFallback);
        }
        turnStartAttempted = true;
        const uploading = attachmentSendControllers.current.get(messageIdForSend);
        if (uploading) uploading.phase = "admitting";
        setAttachmentUploads((existing) => {
          if (!(messageIdForSend in existing)) return existing;
          const next = { ...existing };
          delete next[messageIdForSend];
          return next;
        });
        const admissionKeeper = new AbortController();
        const stagesForAdmission = stagedAttachmentAttempts.get(routeThreadRef)?.attachments ?? [];
        const keeper =
          stagesForAdmission.length > 0
            ? keepStagedAttachmentsAlive(
                { environmentId, input: stagesForAdmission },
                { signal: admissionKeeper.signal },
              )
            : null;
        const inputForAdmission: StartThreadTurnInput = {
          commandId: newCommandId(),
          threadId: threadIdForSend,
          message: {
            messageId: messageIdForSend,
            role: "user",
            text: outgoingMessageText,
            attachments: turnAttachmentsResult.value,
          },
          modelSelection: ctxSelectedModelSelection,
          ...(enqueue ? { queued: true } : {}),
          titleSeed: title,
          runtimeMode,
          interactionMode,
          ...(bootstrap ? { bootstrap } : {}),
          createdAt: messageCreatedAt,
        };
        const admissionInput =
          stagesForAdmission.length > 0 ? structuredClone(inputForAdmission) : inputForAdmission;
        let startResult;
        try {
          startResult = await startThreadTurn({
            environmentId,
            input: attachmentAuthority
              ? { ...admissionInput, attachmentAuthority }
              : admissionInput,
          });
        } finally {
          admissionKeeper.abort();
          if (keeper) await keeper;
        }
        if (startResult._tag === "Failure") {
          failure = startResult;
          const retained = stagedAttachmentAttempts.get(routeThreadRef);
          if (retained && admissionOutcomeIsAmbiguous(startResult.cause))
            stagedAttachmentAttempts.set(routeThreadRef, sendLease, {
              ...retained,
              ambiguousAdmission: { input: admissionInput, restoredDraft: null },
            });
        } else {
          turnStartSucceeded = true;
        }
      }

      attachmentSendControllers.current.delete(messageIdForSend);
      if (currentQueueThreadKeyRef.current === routeThreadKey) {
        setAttachmentUploads((existing) => {
          const next = { ...existing };
          delete next[messageIdForSend];
          return next;
        });
      }
      if (failure !== null) {
        if (enqueue) queuedMessageCache.take(messageIdForSend);
        restoreOutgoing();
        const uncertain = stagedAttachmentAttempts.get(routeThreadRef);
        if (uncertain?.ambiguousAdmission)
          stagedAttachmentAttempts.set(routeThreadRef, sendLease, {
            ...uncertain,
            ambiguousAdmission: {
              ...uncertain.ambiguousAdmission,
              restoredDraft: useComposerDraftStore.getState().getComposerDraft(composerDraftTarget),
            },
          });
        if (
          !isAtomCommandInterrupted(failure) &&
          currentQueueThreadKeyRef.current === routeThreadKey
        ) {
          const error = squashAtomCommandFailure(failure);
          const safeCause =
            isUploadError(error) ||
            isUploadClientError(error) ||
            isUploadEnvironmentUnavailableError(error)
              ? error.message.replace(/\.$/u, "")
              : null;
          setThreadError(
            threadIdForSend,
            stagingFailed
              ? attachmentUploadFailureCopy(uploadFileName, safeCause)
              : error instanceof Error
                ? error.message
                : "Failed to send message.",
          );
        }
      }
      const retained = stagedAttachmentAttempts.get(routeThreadRef);
      if (turnStartSucceeded) {
        stagedAttachmentAttempts.delete(routeThreadRef, sendLease);
      } else if (
        retained &&
        !retained.ambiguousAdmission &&
        (!turnStartAttempted ||
          uploadController.signal.aborted ||
          currentQueueThreadKeyRef.current !== routeThreadKey)
      ) {
        stagedAttachmentAttempts.delete(routeThreadRef, sendLease);
        void releaseStagedAttachments({ environmentId, input: retained.attachments });
      }
      if (
        enqueue &&
        composerAttachmentsSnapshot.length > 0 &&
        currentQueueThreadKeyRef.current === routeThreadKey
      ) {
        setOptimisticUserMessages((existing) =>
          existing.filter((message) => message.id !== messageIdForSend),
        );
      }
      if (enqueue && turnStartSucceeded) {
        for (const attachment of composerAttachmentsSnapshot) {
          if (attachment.type === "image") revokeBlobPreviewUrl(attachment.previewUrl);
        }
      }
      sendInFlightRef.current = false;
      if (!turnStartSucceeded && !enqueue) {
        resetLocalDispatch();
      }
    } finally {
      sendInFlightRef.current = false;
      attachmentAdmissionOwner.finish(routeThreadRef, sendLease);
    }
  };

  const onInterrupt = async () => {
    if (!activeThread || stoppingQueueRef.current) return;
    stoppingQueueRef.current = true;
    try {
      for (const message of queuedMessages) {
        if (message.delivery?.state === "queued") await onCancelQueuedMessage(message.id);
      }

      if (phase !== "running" && cancellableDeliveryThreadId && cancellableDeliveryMessageId) {
        const result = await resolveTurnDelivery({
          environmentId,
          input: {
            threadId: cancellableDeliveryThreadId,
            messageId: cancellableDeliveryMessageId,
            action: "dismiss",
          },
        });
        if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          setThreadError(
            cancellableDeliveryThreadId,
            error instanceof Error ? error.message : "Failed to cancel message delivery.",
          );
        } else {
          resetLocalDispatch();
        }
        return;
      }
      const result = await interruptThreadTurn({
        environmentId,
        input: buildThreadTurnInterruptInput(activeThread),
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        setThreadError(
          activeThread.id,
          error instanceof Error ? error.message : "Failed to interrupt the current turn.",
        );
      }
    } finally {
      stoppingQueueRef.current = false;
    }
  };

  const onRespondToApproval = useCallback(
    async (requestId: ApprovalRequestId, decision: ProviderApprovalDecision) => {
      if (!activeThreadId) return;

      setRespondingRequestIds((existing) =>
        existing.includes(requestId) ? existing : [...existing, requestId],
      );
      const result =
        decision === "cancel" && activeThread
          ? await interruptThreadTurn({
              environmentId,
              input: buildThreadTurnInterruptInput(activeThread),
            })
          : await respondToThreadApproval({
              environmentId,
              input: {
                threadId: activeThreadId,
                requestId,
                decision,
              },
            });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        setThreadError(
          activeThreadId,
          error instanceof Error
            ? error.message
            : decision === "cancel"
              ? "Failed to cancel the current turn."
              : "Failed to submit approval decision.",
        );
      }
      setRespondingRequestIds((existing) => existing.filter((id) => id !== requestId));
      return result;
    },
    [
      activeThread,
      activeThreadId,
      environmentId,
      interruptThreadTurn,
      respondToThreadApproval,
      setThreadError,
    ],
  );

  const onRespondToUserInput = useCallback(
    async (requestId: ApprovalRequestId, answers: Record<string, unknown>) => {
      if (!activeThreadId) return;

      setRespondingUserInputRequestIds((existing) =>
        existing.includes(requestId) ? existing : [...existing, requestId],
      );
      const result = await respondToThreadUserInput({
        environmentId,
        input: {
          threadId: activeThreadId,
          requestId,
          answers,
        },
      });
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        setThreadError(
          activeThreadId,
          error instanceof Error ? error.message : "Failed to submit user input.",
        );
      }
      setRespondingUserInputRequestIds((existing) => existing.filter((id) => id !== requestId));
      return result;
    },
    [activeThreadId, environmentId, respondToThreadUserInput, setThreadError],
  );

  const setActivePendingUserInputQuestionIndex = useCallback(
    (nextQuestionIndex: number) => {
      if (!activePendingUserInput) {
        return;
      }
      setPendingUserInputQuestionIndexByRequestId((existing) => ({
        ...existing,
        [activePendingUserInput.requestId]: nextQuestionIndex,
      }));
    },
    [activePendingUserInput],
  );

  const onSelectActivePendingUserInputOption = useCallback(
    (questionId: string, optionLabel: string) => {
      if (!activePendingUserInput) {
        return;
      }
      const activeQuestion = activePendingProgress?.activeQuestion;
      setPendingUserInputAnswersByRequestId((existing) => {
        const question =
          (activeQuestion?.id === questionId ? activeQuestion : undefined) ??
          activePendingUserInput.questions.find((entry) => entry.id === questionId);
        if (!question) {
          return existing;
        }

        return {
          ...existing,
          [activePendingUserInput.requestId]: {
            ...existing[activePendingUserInput.requestId],
            [questionId]: togglePendingUserInputOptionSelection(
              question,
              existing[activePendingUserInput.requestId]?.[questionId],
              optionLabel,
            ),
          },
        };
      });
      promptRef.current = "";
      readComposerHandle(composerRef)?.resetCursorState({ cursor: 0 });
    },
    [activePendingProgress?.activeQuestion, activePendingUserInput, composerRef],
  );

  const onChangeActivePendingUserInputCustomAnswer = useCallback(
    (
      questionId: string,
      value: string,
      nextCursor: number,
      expandedCursor: number,
      _cursorAdjacentToMention: boolean,
    ) => {
      if (!activePendingUserInput) {
        return;
      }
      promptRef.current = value;
      setPendingUserInputAnswersByRequestId((existing) => ({
        ...existing,
        [activePendingUserInput.requestId]: {
          ...existing[activePendingUserInput.requestId],
          [questionId]: setPendingUserInputCustomAnswer(
            existing[activePendingUserInput.requestId]?.[questionId],
            value,
          ),
        },
      }));
      const composer = readComposerHandle(composerRef);
      const snapshot = composer?.readSnapshot();
      if (
        snapshot?.value !== value ||
        snapshot.cursor !== nextCursor ||
        snapshot.expandedCursor !== expandedCursor
      ) {
        composer?.focusAt(nextCursor);
      }
    },
    [activePendingUserInput, composerRef],
  );

  const onAdvanceActivePendingUserInput = useCallback(() => {
    if (!activePendingUserInput || !activePendingProgress) {
      return;
    }
    if (activePendingProgress.isLastQuestion) {
      if (activePendingResolvedAnswers) {
        void onRespondToUserInput(activePendingUserInput.requestId, activePendingResolvedAnswers);
      }
      return;
    }
    setActivePendingUserInputQuestionIndex(activePendingProgress.questionIndex + 1);
  }, [
    activePendingProgress,
    activePendingResolvedAnswers,
    activePendingUserInput,
    onRespondToUserInput,
    setActivePendingUserInputQuestionIndex,
  ]);

  const onPreviousActivePendingUserInputQuestion = useCallback(() => {
    if (!activePendingProgress) {
      return;
    }
    setActivePendingUserInputQuestionIndex(Math.max(activePendingProgress.questionIndex - 1, 0));
  }, [activePendingProgress, setActivePendingUserInputQuestionIndex]);

  const onSubmitPlanFollowUp = useCallback(
    async ({
      text,
      interactionMode: nextInteractionMode,
    }: {
      text: string;
      interactionMode: "default" | "plan";
    }) => {
      if (
        !activeThread ||
        !isServerThread ||
        isSendBusy ||
        isConnecting ||
        sendInFlightRef.current
      ) {
        return;
      }

      const trimmed = text.trim();
      if (!trimmed) {
        return;
      }

      const sendCtx = composerRef.current?.getSendContext();
      if (!sendCtx) {
        return;
      }
      const {
        selectedProvider: ctxSelectedProvider,
        selectedModel: ctxSelectedModel,
        selectedProviderModels: ctxSelectedProviderModels,
        selectedPromptEffort: ctxSelectedPromptEffort,
        selectedModelSelection: ctxSelectedModelSelection,
      } = sendCtx;

      const threadIdForSend = activeThread.id;
      const messageIdForSend = newMessageId();
      const messageCreatedAt = new Date().toISOString();
      const outgoingMessageText = formatOutgoingPrompt({
        provider: ctxSelectedProvider,
        model: ctxSelectedModel,
        models: ctxSelectedProviderModels,
        effort: ctxSelectedPromptEffort,
        text: trimmed,
      });

      sendInFlightRef.current = true;
      beginLocalDispatch({
        preparingWorktree: false,
        threadId: threadIdForSend,
        messageId: messageIdForSend,
      });
      setThreadError(threadIdForSend, null);

      // Position this sent row once LegendList has measured the anchored tail.
      isAtEndRef.current = true;
      timelineScrollModeRef.current = "anchoring-new-turn";
      liveFollowUserScrollGenerationRef.current = anchorUserScrollGenerationRef.current;
      pendingTimelineAnchorRef.current = messageIdForSend;
      activeTimelineAnchorIndexRef.current = null;
      showScrollDebouncer.current.cancel();
      setScrollPillThreadId(null);
      setTimelineAnchor({
        threadKey: scopedThreadKey(scopeThreadRef(activeThread.environmentId, threadIdForSend)),
        messageId: messageIdForSend,
      });

      setOptimisticUserMessages((existing) => [
        ...existing,
        {
          id: messageIdForSend,
          role: "user",
          text: outgoingMessageText,
          turnId: null,
          createdAt: messageCreatedAt,
          updatedAt: messageCreatedAt,
          streaming: false,
        },
      ]);

      const settingsResult = await persistThreadSettingsForNextTurn({
        threadId: threadIdForSend,
        createdAt: messageCreatedAt,
        modelSelection: ctxSelectedModelSelection,
        runtimeMode,
        interactionMode: nextInteractionMode,
      });
      let failure: AtomCommandResult<unknown, unknown> | null =
        settingsResult._tag === "Failure" ? settingsResult : null;

      if (failure === null) {
        // Keep the mode toggle and plan-follow-up banner in sync immediately
        // while the same-thread implementation turn is starting.
        setComposerDraftInteractionMode(
          scopeThreadRef(activeThread.environmentId, threadIdForSend),
          nextInteractionMode,
        );

        const startResult = await startThreadTurn({
          environmentId,
          input: {
            threadId: threadIdForSend,
            message: {
              messageId: messageIdForSend,
              role: "user",
              text: outgoingMessageText,
              attachments: [],
            },
            modelSelection: ctxSelectedModelSelection,
            titleSeed: activeThread.title,
            runtimeMode,
            interactionMode: nextInteractionMode,
            ...(nextInteractionMode === "default" && activeProposedPlan
              ? {
                  sourceProposedPlan: {
                    threadId: activeThread.id,
                    planId: activeProposedPlan.id,
                  },
                }
              : {}),
            createdAt: messageCreatedAt,
          },
        });
        failure = startResult._tag === "Failure" ? startResult : null;
      }

      if (failure === null) {
        // Optimistically open the plan sidebar when implementing (not refining).
        // "default" mode here means the agent is executing the plan, which produces
        // step-tracking activities that the sidebar will display.
        if (nextInteractionMode === "default" && autoOpenPlanSidebar) {
          planSidebarDismissedForTurnRef.current = null;
          if (activeThreadRef) {
            useRightPanelStore.getState().open(activeThreadRef, "plan");
          }
        }
        sendInFlightRef.current = false;
        return;
      }

      setOptimisticUserMessages((existing) =>
        existing.filter((message) => message.id !== messageIdForSend),
      );
      if (!isAtomCommandInterrupted(failure)) {
        const error = squashAtomCommandFailure(failure);
        setThreadError(
          threadIdForSend,
          error instanceof Error ? error.message : "Failed to send plan follow-up.",
        );
      }
      sendInFlightRef.current = false;
      resetLocalDispatch();
    },
    [
      activeThread,
      activeThreadRef,
      activeProposedPlan,
      beginLocalDispatch,
      isConnecting,
      isSendBusy,
      isServerThread,
      persistThreadSettingsForNextTurn,
      resetLocalDispatch,
      runtimeMode,
      setComposerDraftInteractionMode,
      setThreadError,
      startThreadTurn,
      autoOpenPlanSidebar,
      environmentId,
      composerRef,
    ],
  );

  const onImplementPlanInNewThread = useCallback(async () => {
    if (
      !activeThread ||
      !activeProject ||
      !activeProposedPlan ||
      !isServerThread ||
      isSendBusy ||
      isConnecting ||
      activeEnvironmentUnavailable ||
      workspaceUnavailable !== null ||
      sendInFlightRef.current
    ) {
      return;
    }

    const sendCtx = composerRef.current?.getSendContext();
    if (!sendCtx) {
      return;
    }
    const {
      selectedProvider: ctxSelectedProvider,
      selectedModel: ctxSelectedModel,
      selectedProviderModels: ctxSelectedProviderModels,
      selectedPromptEffort: ctxSelectedPromptEffort,
      selectedModelSelection: ctxSelectedModelSelection,
    } = sendCtx;

    const createdAt = new Date().toISOString();
    const nextThreadId = newThreadId();
    const nextMessageId = newMessageId();
    const planMarkdown = activeProposedPlan.planMarkdown;
    const implementationPrompt = buildPlanImplementationPrompt(planMarkdown);
    const outgoingImplementationPrompt = formatOutgoingPrompt({
      provider: ctxSelectedProvider,
      model: ctxSelectedModel,
      models: ctxSelectedProviderModels,
      effort: ctxSelectedPromptEffort,
      text: implementationPrompt,
    });
    const nextThreadTitle = truncate(buildPlanImplementationThreadTitle(planMarkdown));
    const nextThreadModelSelection: ModelSelection = ctxSelectedModelSelection;

    sendInFlightRef.current = true;
    beginLocalDispatch({
      preparingWorktree: false,
      threadId: nextThreadId,
      messageId: nextMessageId,
    });
    const finish = () => {
      sendInFlightRef.current = false;
      resetLocalDispatch();
    };

    const createResult = await createPanelThread({
      environmentId,
      input: {
        commandId: newCommandId(),
        hostThreadId: activeThread.id,
        threadId: nextThreadId,
        title: nextThreadTitle,
        threadDefaults: {
          modelSelection: nextThreadModelSelection,
          runtimeMode,
          interactionMode: "default",
        },
      },
    });
    let failure: AtomCommandResult<unknown, unknown> | null =
      createResult._tag === "Failure" ? createResult : null;

    if (failure === null) {
      const startResult = await startThreadTurn({
        environmentId,
        input: {
          threadId: nextThreadId,
          message: {
            messageId: nextMessageId,
            role: "user",
            text: outgoingImplementationPrompt,
            attachments: [],
          },
          modelSelection: ctxSelectedModelSelection,
          titleSeed: nextThreadTitle,
          runtimeMode,
          interactionMode: "default",
          sourceProposedPlan: {
            threadId: activeThread.id,
            planId: activeProposedPlan.id,
          },
          createdAt,
        },
      });
      failure = startResult._tag === "Failure" ? startResult : null;
    }

    if (failure === null) {
      const startedResult = await settlePromise(() =>
        waitForStartedServerThread(scopeThreadRef(activeThread.environmentId, nextThreadId)),
      );
      failure = startedResult._tag === "Failure" ? startedResult : null;
    }

    if (failure === null) {
      // Signal that the plan sidebar should open on the new thread when enabled.
      planSidebarOpenOnNextThreadRef.current = autoOpenPlanSidebar;
      const navigateResult = await settlePromise(() =>
        navigate({
          to: "/$environmentId/$threadId",
          params: {
            environmentId: activeThread.environmentId,
            threadId: nextThreadId,
          },
        }),
      );
      failure = navigateResult._tag === "Failure" ? navigateResult : null;
    }

    if (failure !== null) {
      const cleanupResult = await deleteThread({
        environmentId,
        input: {
          threadId: nextThreadId,
        },
      });
      if (cleanupResult._tag === "Failure" && !isAtomCommandInterrupted(cleanupResult)) {
        console.warn(
          "Failed to clean up implementation thread after start failure.",
          squashAtomCommandFailure(cleanupResult),
        );
      }
      if (!isAtomCommandInterrupted(failure)) {
        const error = squashAtomCommandFailure(failure);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not start implementation thread",
            description:
              error instanceof Error
                ? error.message
                : "An error occurred while creating the new thread.",
          }),
        );
      }
    }
    finish();
  }, [
    activeProject,
    activeProposedPlan,
    activeThread,
    beginLocalDispatch,
    activeEnvironmentUnavailable,
    workspaceUnavailable,
    createPanelThread,
    deleteThread,
    isConnecting,
    isSendBusy,
    isServerThread,
    navigate,
    resetLocalDispatch,
    runtimeMode,
    startThreadTurn,
    autoOpenPlanSidebar,
    environmentId,
    composerRef,
  ]);

  const getModelDisabledReason = useCallback(
    (instanceId: ProviderInstanceId, model: string): string | null => {
      if (!activeThread) {
        return null;
      }
      const reason = getStartedThreadModelChangeBlockReason({
        providers: providerStatuses,
        hasStartedSession: activeThread.session !== null,
        currentModelSelection: activeThread.modelSelection,
        currentProviderInstanceId: activeThread.session?.providerInstanceId ?? null,
        nextModelSelection: { instanceId, model },
      });
      return reason ? `${reason.description} Start a new thread to use this model.` : null;
    },
    [activeThread, providerStatuses],
  );

  const onProviderModelSelect = useCallback(
    (instanceId: ProviderInstanceId, model: string) => {
      if (!activeThread) return;
      if (providerBinding.conflict !== null) {
        scheduleComposerFocus();
        return;
      }
      if (lockedProviderInstanceId && instanceId !== lockedProviderInstanceId) {
        scheduleComposerFocus();
        return;
      }
      // Look up the configured instance so model normalization and custom
      // model lookup stay scoped to that exact instance. Unknown instance ids
      // are rejected by returning early; the server remains authoritative too.
      const entry = providerStatuses.find((snapshot) => snapshot.instanceId === instanceId);
      const resolvedDriverKind = entry?.driver ?? null;
      if (
        lockedProvider !== null &&
        resolvedDriverKind !== null &&
        resolvedDriverKind !== lockedProvider
      ) {
        scheduleComposerFocus();
        return;
      }
      if (lockedProvider !== null && activeThread.session?.providerInstanceId) {
        const currentEntry = providerStatuses.find(
          (snapshot) => snapshot.instanceId === activeThread.session?.providerInstanceId,
        );
        if (
          currentEntry?.continuation?.groupKey &&
          entry?.continuation?.groupKey &&
          currentEntry.continuation.groupKey !== entry.continuation.groupKey
        ) {
          scheduleComposerFocus();
          return;
        }
      }
      const resolvedModel = resolveAppModelSelectionForInstance(
        instanceId,
        settings,
        providerStatuses,
        model,
      );
      if (!resolvedModel) {
        scheduleComposerFocus();
        return;
      }
      const nextModelSelection: ModelSelection = {
        instanceId,
        model: resolvedModel,
      };
      const modelChangeBlockReason = getStartedThreadModelChangeBlockReason({
        providers: providerStatuses,
        hasStartedSession: activeThread.session !== null,
        currentModelSelection: activeThread.modelSelection,
        currentProviderInstanceId: activeThread.session?.providerInstanceId ?? null,
        nextModelSelection,
      });
      if (modelChangeBlockReason) {
        toastManager.add({
          type: "warning",
          title: modelChangeBlockReason.title,
          description: modelChangeBlockReason.description,
        });
        scheduleComposerFocus();
        return;
      }
      setComposerDraftModelSelection(
        scopeThreadRef(activeThread.environmentId, activeThread.id),
        nextModelSelection,
      );
      setStickyComposerModelSelection(nextModelSelection);
      scheduleComposerFocus();
    },
    [
      activeThread,
      lockedProvider,
      lockedProviderInstanceId,
      scheduleComposerFocus,
      setComposerDraftModelSelection,
      setStickyComposerModelSelection,
      providerStatuses,
      providerBinding.conflict,
      settings,
    ],
  );
  const onExpandTimelineImage = useCallback((preview: ExpandedImagePreview) => {
    setExpandedImage(preview);
  }, []);
  const onOpenTurnDiff = useCallback(
    (turnId: TurnId, filePath?: string) => {
      if (!isServerThread || !activeThreadRef) return;
      useDiffPanelStore.getState().selectTurn(activeThreadRef, turnId, filePath);
      useRightPanelStore.getState().open(activeThreadRef, "diff");
      onDiffPanelOpen?.();
    },
    [activeThreadRef, isServerThread, onDiffPanelOpen],
  );
  // Both the Map and the revert handler are read from refs at call-time so
  // the callback reference is fully stable and never busts context identity.
  const revertTurnCountRef = useRef(revertTurnCountByUserMessageId);
  const onRevertToTurnCountRef = useRef(onRevertToTurnCount);
  useLayoutEffect(() => {
    revertTurnCountRef.current = revertTurnCountByUserMessageId;
    onRevertToTurnCountRef.current = onRevertToTurnCount;
  }, [onRevertToTurnCount, revertTurnCountByUserMessageId]);
  const onRevertUserMessage = useCallback((messageId: MessageId) => {
    const targetTurnCount = revertTurnCountRef.current.get(messageId);
    if (typeof targetTurnCount !== "number") {
      return;
    }
    void onRevertToTurnCountRef.current(targetTurnCount);
  }, []);

  // Empty state: no active thread
  if (!activeThread) {
    return isPanel ? <ChatPanelOpeningState /> : <NoActiveThreadState />;
  }

  const panelToggleControls = (
    <PanelLayoutControls
      rightPanelAvailable={activeProject !== null}
      rightPanelOpen={effectiveRightPanelOpen}
      rightPanelShortcutLabel={shortcutLabelForCommand(keybindings, "rightPanel.toggle")}
      onToggleRightPanel={toggleRightPanel}
    />
  );
  const panelLayoutControls = (
    <div className="workspace-titlebar-controls z-50 gap-1 [-webkit-app-region:no-drag]">
      {effectiveRightPanelOpen && !shouldUsePlanSidebarSheet ? (
        <RightPanelMaximizeControl
          maximized={rightPanelMaximized}
          onToggle={toggleRightPanelMaximized}
        />
      ) : null}
      {panelToggleControls}
    </div>
  );
  const rightPanelContent = activeThreadRef ? (
    activeRightPanelSurface?.kind === "preview" ? (
      <Suspense fallback={null}>
        <PreviewPanel
          mode="embedded"
          threadRef={activeThreadRef}
          tabId={activeRightPanelSurface.resourceId}
          configuredUrls={configuredPreviewUrls}
          visible
        />
      </Suspense>
    ) : activeRightPanelSurface?.kind === "terminal" ? (
      <PersistentThreadTerminalPanel
        threadRef={activeThreadRef}
        surface={activeRightPanelSurface}
        launchContext={null}
        focusRequestId={terminalFocusRequestId}
        keybindings={keybindings}
        onAddTerminalContext={addTerminalContextToDraft}
        onSplitTerminal={splitPanelTerminal}
        onSplitTerminalVertical={splitPanelTerminalVertical}
        onNewTerminal={addTerminalSurface}
        onActiveTerminalChange={activatePanelTerminal}
        onCloseTerminal={closePanelTerminal}
        splitShortcutLabel={splitTerminalShortcutLabel ?? undefined}
        splitVerticalShortcutLabel={splitTerminalVerticalShortcutLabel ?? undefined}
        newShortcutLabel={newTerminalShortcutLabel ?? undefined}
        closeShortcutLabel={closeTerminalShortcutLabel ?? undefined}
        workspaceUnavailable={workspaceUnavailable}
      />
    ) : activeRightPanelSurface?.kind === "diff" ? (
      <Suspense fallback={null}>
        <DiffPanel
          mode="embedded"
          composerDraftTarget={composerDraftTarget}
          thread={activeThread}
          workspaceUnavailable={workspaceUnavailable}
        />
      </Suspense>
    ) : activeRightPanelSurface?.kind === "sourceControl" ? (
      <Suspense fallback={null}>
        <SourceControlPanel
          key={scopedThreadKey(activeThreadRef)}
          mode="embedded"
          threadRef={activeThreadRef}
          gitCwd={gitCwd}
          projectRef={activeProjectRef}
          workspaceUnavailable={workspaceUnavailable}
        />
      </Suspense>
    ) : activeRightPanelSurface?.kind === "device" ? (
      <Suspense fallback={null}>
        <DevicePanel environmentId={activeThreadRef.environmentId} />
      </Suspense>
    ) : activeRightPanelSurface?.kind === "pullRequest" ? (
      <Suspense fallback={null}>
        <PullRequestPanelSurface
          key={`${scopedThreadKey(activeThreadRef)}:${activeRightPanelSurface.id}`}
          threadRef={activeThreadRef}
          projectRef={activeProjectRef}
          gitCwd={gitCwd}
          workspaceUnavailable={workspaceUnavailable}
          surface={activeRightPanelSurface}
        />
      </Suspense>
    ) : activeRightPanelSurface?.kind === "plan" ? (
      <PlanSidebar
        activePlan={activePlan}
        activeProposedPlan={sidebarProposedPlan}
        label={planSidebarLabel}
        environmentId={environmentId}
        threadRef={activeThreadRef}
        markdownCwd={gitCwd ?? undefined}
        workspaceRoot={activeWorkspaceRoot}
        timestampFormat={timestampFormat}
        mode="embedded"
      />
    ) : activeActivitySurface !== null &&
      activityStateTarget !== null &&
      activeThreadRef !== null ? (
      <ActivityPanelBinding
        target={activityStateTarget}
        threadRef={activeThreadRef}
        surface={activeActivitySurface}
        timestampFormat={timestampFormat}
      />
    ) : (activeRightPanelSurface?.kind === "files" || activeRightPanelSurface?.kind === "file") &&
      activeProject &&
      activeWorkspaceRoot ? (
      <Suspense fallback={null}>
        <FilePreviewPanel
          key={filePreviewViewKey}
          environmentId={activeProject.environmentId}
          cwd={activeWorkspaceRoot}
          projectName={activeProject.title}
          threadRef={activeThreadRef}
          composerDraftTarget={composerDraftTarget}
          keybindings={keybindings}
          availableEditors={availableEditors}
          relativePath={
            activeRightPanelSurface.kind === "file" ? activeRightPanelSurface.relativePath : null
          }
          revealLine={activeFileSurface?.revealLine ?? null}
          revealRequestId={activeFileSurface?.revealRequestId ?? 0}
          onOpenFile={openFileSurface}
          onPendingChange={handleFilePendingChange}
          editingSessions={fileEditingSessions}
          workspaceUnavailable={workspaceUnavailable}
        />
      </Suspense>
    ) : null
  ) : null;

  return (
    <div className="relative flex min-h-0 min-w-0 flex-1 overflow-hidden bg-background">
      {activeThreadRef && activeProject && activeWorkspaceRoot ? (
        <ProjectFilesPreloader
          environmentId={activeProject.environmentId}
          cwd={activeWorkspaceRoot}
        />
      ) : null}
      {!isPanel && activeThreadRef ? (
        <DesktopPreviewTabHosts
          threadRef={activeThreadRef}
          surfaces={rightPanelState.surfaces}
          sessions={activePreviewState.sessions}
          activeSurfaceId={
            effectiveRightPanelOpen ? (rightPanelState.activeSurfaceId ?? null) : null
          }
        />
      ) : null}
      {!isPanel ? panelLayoutControls : null}
      <div
        className={cn(
          "flex min-h-0 min-w-0 flex-col overflow-x-hidden",
          rightPanelMaximized ? "w-0 flex-none" : "flex-1",
        )}
        data-chat-column-maximized-away={rightPanelMaximized ? "true" : "false"}
      >
        {(() => {
          const focusedGroupEdges = findCenterPanelGroupEdges(
            centerPanelState.layout,
            centerPanelState.focusedGroupId,
          );
          const reserveCenterTitlebarControls =
            !effectiveRightPanelOpen && focusedGroupEdges?.top === true && focusedGroupEdges.right;
          const hostChatSurfaceBody = (
            <>
              <ProviderStatusBanner status={activeProviderStatus} />
              <ThreadErrorBanner
                error={threadError}
                attribution={threadErrorAttributionText}
                onDismiss={() => setThreadError(activeThread.id, null)}
              />
              <div className="flex min-h-0 min-w-0 flex-1">
                {/* Chat column */}
                <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
                  {/* Messages Wrapper */}
                  <div className="relative flex min-h-0 flex-1 flex-col">
                    {activityStateTarget !== null &&
                    activeThreadRef !== null &&
                    activeThread !== null ? (
                      <ActivityDockBinding
                        target={activityStateTarget}
                        threadRef={activeThreadRef}
                        projectId={activeThread.projectId}
                        compact={shouldUseCompactActivityDock}
                        avoidRightPanelSheet={shouldUsePlanSidebarSheet && effectiveRightPanelOpen}
                      />
                    ) : null}
                    {/* Messages — LegendList handles virtualization and scrolling internally */}
                    <MessagesTimeline
                      key={activeThread.id}
                      isWorking={isWorking}
                      activeTurnInProgress={isWorking || !latestTurnSettled}
                      activeTurnStartedAt={activeWorkStartedAt}
                      listRef={legendListRef}
                      timelineEntries={timelineEntries}
                      queuedMessages={queuedMessages}
                      queuedStatuses={queuedStatuses}
                      onSteerQueuedMessage={onSteerQueuedMessage}
                      onSendNowQueuedMessage={onSendNowQueuedMessage}
                      onCancelQueuedMessage={onCancelQueuedMessage}
                      resolvingQueuedMessageId={resolvingQueuedMessageId}
                      queuedMessageErrors={queuedMessageErrors}
                      attachmentUploads={activeAttachmentUploads}
                      onCancelAttachmentUpload={onCancelAttachmentUpload}
                      latestTurn={activeLatestTurn}
                      runningTurnId={
                        activeThread.session?.status === "running"
                          ? activeThread.session.activeTurnId
                          : null
                      }
                      turnDiffSummaryByAssistantMessageId={turnDiffSummaryByAssistantMessageId}
                      activeThreadEnvironmentId={activeThread.environmentId}
                      routeThreadKey={routeThreadKey}
                      onOpenTurnDiff={onOpenTurnDiff}
                      revertTurnCountByUserMessageId={revertTurnCountByUserMessageId}
                      onRevertUserMessage={onRevertUserMessage}
                      onResolveTurnDelivery={onResolveTurnDelivery}
                      resolvingTurnDeliveryMessageId={resolvingTurnDeliveryMessageId}
                      blockingDelivery={blockingDelivery}
                      instanceLabels={instanceLabels}
                      isRevertingCheckpoint={isRevertingCheckpoint}
                      onImageExpand={onExpandTimelineImage}
                      markdownCwd={gitCwd ?? undefined}
                      resolvedTheme={resolvedTheme}
                      timestampFormat={timestampFormat}
                      workspaceRoot={activeWorkspaceRoot}
                      skills={activeProviderStatus?.skills ?? EMPTY_PROVIDER_SKILLS}
                      anchorMessageId={timelineAnchorMessageId}
                      onAnchorReady={onTimelineAnchorReady}
                      onAnchorSizeChanged={onTimelineAnchorSizeChanged}
                      contentInsetEndAdjustment={composerOverlayHeight}
                      onIsAtEndChange={onIsAtEndChange}
                      onManualNavigation={cancelTimelineLiveFollowForUserNavigation}
                    />

                    {/* scroll to end pill — shown when user has scrolled away from the live edge */}
                    {showScrollToBottom && (
                      <div
                        className="pointer-events-none absolute left-1/2 z-30 flex -translate-x-1/2 justify-center py-1.5"
                        style={{ bottom: composerOverlayHeight + 4 }}
                      >
                        <button
                          type="button"
                          aria-label="Scroll to end"
                          title="Scroll to end"
                          onClick={() => scrollToEnd(true)}
                          className="pointer-events-auto flex items-center gap-1.5 rounded-full border border-border/60 bg-card px-3 py-1 text-muted-foreground text-xs shadow-sm transition-colors hover:border-border hover:text-foreground hover:cursor-pointer"
                        >
                          <ChevronDownIcon className="size-3.5" />
                          Scroll to end
                        </button>
                      </div>
                    )}
                  </div>

                  {/* Input bar */}
                  <div
                    ref={setComposerOverlayElement}
                    data-chat-composer-overlay="true"
                    className="pointer-events-none absolute inset-x-0 bottom-0 z-20 pt-1.5 sm:pt-2"
                  >
                    <div
                      aria-hidden="true"
                      className="chat-composer-horizontal-inset pointer-events-none absolute inset-x-0 top-1.5 bottom-0 z-0 sm:top-2"
                    >
                      <div className="relative mx-auto h-full w-full max-w-3xl overflow-clip rounded-t-[20px]">
                        <div className="chat-composer-shared-blur absolute -inset-8" />
                      </div>
                    </div>
                    <div className="chat-composer-horizontal-inset">
                      <div className="pointer-events-auto relative z-10 isolate">
                        <ComposerBannerStack className="relative z-0" items={composerBannerItems} />
                        <div className="relative z-10">
                          <ChatComposer
                            composerRef={composerRef}
                            composerDraftTarget={composerDraftTarget}
                            environmentId={environmentId}
                            routeKind={routeKind}
                            routeThreadRef={routeThreadRef}
                            draftId={draftId}
                            activeThreadId={activeThreadId}
                            activeThreadEnvironmentId={activeThread?.environmentId}
                            activeThread={activeThread}
                            isServerThread={isServerThread}
                            isLocalDraftThread={isLocalDraftThread}
                            phase={phase}
                            isConnecting={isConnecting}
                            isSendBusy={
                              isSendBusy || hasActiveAttachmentUpload || applicationSendBusy
                            }
                            canCancelPendingSend={canCancelPendingSend}
                            isPreparingWorktree={isPreparingWorktree}
                            environmentUnavailable={activeEnvironmentUnavailableState}
                            activePendingApproval={activePendingApproval}
                            pendingApprovals={pendingApprovals}
                            pendingUserInputs={pendingUserInputs}
                            activePendingProgress={activePendingProgress}
                            activePendingResolvedAnswers={activePendingResolvedAnswers}
                            activePendingIsResponding={activePendingIsResponding}
                            activePendingDraftAnswers={activePendingDraftAnswers}
                            activePendingQuestionIndex={activePendingQuestionIndex}
                            respondingRequestIds={respondingRequestIds}
                            showPlanFollowUpPrompt={showPlanFollowUpPrompt}
                            activeProposedPlan={activeProposedPlan}
                            activePlan={activePlan as { turnId?: TurnId } | null}
                            sidebarProposedPlan={sidebarProposedPlan as { turnId?: TurnId } | null}
                            planSidebarLabel={planSidebarLabel}
                            planSidebarOpen={planSidebarOpen}
                            runtimeMode={runtimeMode}
                            interactionMode={interactionMode}
                            lockedProvider={lockedProvider}
                            providerBindingInstanceId={providerBinding.instanceId}
                            lockProviderPickerToActiveInstance={lockProviderPickerToActiveInstance}
                            providerBindingConflictReason={
                              workspaceUnavailable ?? providerBindingConflictReason
                            }
                            providerStatuses={providerStatuses as ServerProvider[]}
                            activeProjectDefaultModelSelection={
                              activeProject?.defaultModelSelection
                            }
                            activeThreadModelSelection={activeThread?.modelSelection}
                            {...(routeKind === "server"
                              ? { onCommitModelSelection: commitComposerModelSelection }
                              : {})}
                            activeThreadActivities={activeThread?.activities}
                            resolvedTheme={resolvedTheme}
                            settings={settings}
                            keybindings={keybindings}
                            gitCwd={gitCwd}
                            promptRef={promptRef}
                            composerAttachmentsRef={composerAttachmentsRef}
                            composerTerminalContextsRef={composerTerminalContextsRef}
                            composerElementContextsRef={composerElementContextsRef}
                            onSend={onSend}
                            onInterrupt={onInterrupt}
                            onSteerQueuedMessage={onSteerHeadQueuedMessage}
                            onImplementPlanInNewThread={onImplementPlanInNewThread}
                            onRespondToApproval={onRespondToApproval}
                            onSelectActivePendingUserInputOption={
                              onSelectActivePendingUserInputOption
                            }
                            onAdvanceActivePendingUserInput={onAdvanceActivePendingUserInput}
                            onPreviousActivePendingUserInputQuestion={
                              onPreviousActivePendingUserInputQuestion
                            }
                            onChangeActivePendingUserInputCustomAnswer={
                              onChangeActivePendingUserInputCustomAnswer
                            }
                            onProviderModelSelect={onProviderModelSelect}
                            getModelDisabledReason={getModelDisabledReason}
                            toggleInteractionMode={toggleInteractionMode}
                            handleRuntimeModeChange={handleRuntimeModeChange}
                            handleInteractionModeChange={handleInteractionModeChange}
                            togglePlanSidebar={togglePlanSidebar}
                            focusComposer={focusComposer}
                            scheduleComposerFocus={scheduleComposerFocus}
                            setThreadError={setThreadError}
                            onExpandImage={onExpandTimelineImage}
                          />
                        </div>
                      </div>
                    </div>
                    <div
                      className={cn(
                        "chat-composer-horizontal-inset chat-composer-lower-chrome relative z-10",
                        "pb-[calc(env(safe-area-inset-bottom)+0.75rem)] sm:pb-[calc(env(safe-area-inset-bottom)+1rem)]",
                      )}
                    />
                  </div>

                  {pullRequestDialogState ? (
                    <PullRequestThreadDialog
                      key={pullRequestDialogState.key}
                      open
                      environmentId={activeThread.environmentId}
                      cwd={activeProject?.workspaceRoot ?? null}
                      initialReference={pullRequestDialogState.initialReference}
                      onOpenChange={(open) => {
                        if (!open) {
                          closePullRequestDialog();
                        }
                      }}
                      onPrepared={handlePreparedPullRequestThread}
                    />
                  ) : null}
                </div>
                {/* end chat column */}
              </div>
              {/* end horizontal flex container */}
            </>
          );

          return !isPanel && activeThreadRef ? (
            <LiveCenterPanelWorkspace
              workspaceRef={centerPanelWorkspaceRef}
              state={centerPanelState}
              hostLabel={centerHostLabel}
              terminalLabelsById={activeTerminalLabelsById}
              renderFocusedActions={(density) => (
                <ChatHeaderActions
                  density={density}
                  activeThreadEnvironmentId={activeThread.environmentId}
                  activeThreadId={activeThread.id}
                  {...(routeKind === "draft" && draftId ? { draftId } : {})}
                  activeProjectName={activeProject?.title}
                  openInCwd={gitCwd}
                  activeProjectScripts={activeProject?.scripts}
                  preferredScriptId={
                    activeProject ? (lastInvokedScriptByProjectId[activeProject.id] ?? null) : null
                  }
                  keybindings={keybindings}
                  availableEditors={availableEditors}
                  reserveTitlebarControls={reserveCenterTitlebarControls}
                  gitCwd={gitCwd}
                  providerStatuses={providerStatuses as ServerProvider[]}
                  settings={settings}
                  canCreatePanel={centerPanelLaunchContext !== null}
                  onCreateChatPanel={handleCreateChatPanel}
                  onReopenChatPanel={handleReopenChatPanel}
                  onOpenTerminalPanel={handleOpenTerminalPanel}
                  onOpenProviderTerminalPanel={handleOpenProviderTerminalPanel}
                  onRunProjectScript={runProjectScript}
                  onAddProjectScript={saveProjectScript}
                  onUpdateProjectScript={updateProjectScript}
                  onDeleteProjectScript={deleteProjectScript}
                  workspaceUnavailable={workspaceUnavailable}
                />
              )}
              hostChatSurfaceBody={hostChatSurfaceBody}
              hostThread={activeThread}
              hostThreadRef={activeThreadRef}
              centerTerminalLaunchContext={centerTerminalLaunchContext}
              keybindings={keybindings}
              terminalFocusRequestId={terminalFocusRequestId}
              onAddTerminalContext={addTerminalContextToDraft}
              onFocusGroup={focusCenterPanelGroup}
              onActivate={activateCenterPanelSurface}
              onCloseSurface={closeCenterPanelSurface}
              onCloseOtherSurfaces={closeOtherCenterPanelSurfaces}
              onCloseSurfacesToRight={closeCenterPanelSurfacesToRight}
              onCloseAllSurfaces={closeAllCenterPanelSurfaces}
              onDropSurface={dropCenterPanelSurface}
              onMergeGroup={mergeCenterPanelGroup}
              onSetSplitRatio={setCenterPanelSplitRatio}
              workspaceUnavailable={workspaceUnavailable}
            />
          ) : (
            hostChatSurfaceBody
          );
        })()}
      </div>

      {effectiveRightPanelOpen && !shouldUsePlanSidebarSheet && activeThreadRef ? (
        <RightPanelTabs
          mode="inline"
          maximized={rightPanelMaximized}
          allowAddSurfaces={!isPanel}
          surfaces={renderedRightPanelSurfaces}
          activeSurfaceId={activeRightPanelSurface?.id ?? null}
          pendingSurfaceIds={pendingFileSurfaceIds}
          previewSessions={activePreviewState.sessions}
          previewDesktopByTabId={activePreviewState.desktopByTabId}
          terminalLabelsById={activeTerminalLabelsById}
          onActivate={activateRightPanelSurface}
          onCloseSurface={closeDisplayedRightPanelSurface}
          onCloseOtherSurfaces={closeOtherDisplayedRightPanelSurfaces}
          onCloseSurfacesToRight={closeDisplayedRightPanelSurfacesToRight}
          onCloseAllSurfaces={closeAllDisplayedRightPanelSurfaces}
          onCopyFilePath={copyRightPanelFilePath}
          onAddBrowser={createBrowserSurface}
          onAddTerminal={addTerminalSurface}
          onAddDiff={addDiffSurface}
          onAddSourceControl={addSourceControlSurface}
          onAddFiles={addFilesSurface}
          onAddDevice={onAddDevice}
          browserAvailable={isPreviewSupportedInRuntime()}
          diffAvailable={gitRightPanelAvailable}
          sourceControlAvailable={gitRightPanelAvailable}
          filesAvailable={activeProject !== null}
        >
          {rightPanelContent}
        </RightPanelTabs>
      ) : null}
      {effectiveRightPanelOpen && shouldUsePlanSidebarSheet && activeThreadRef ? (
        <RightPanelSheet
          open
          onClose={planSidebarOpen ? closePlanSidebar : closePreviewPanel}
          consumeEscapeClose={consumeActivityDockEscapeClose}
        >
          <RightPanelTabs
            mode="sheet"
            {...(!isPanel ? { layoutControls: panelToggleControls } : {})}
            allowAddSurfaces={!isPanel}
            surfaces={renderedRightPanelSurfaces}
            activeSurfaceId={activeRightPanelSurface?.id ?? null}
            pendingSurfaceIds={pendingFileSurfaceIds}
            previewSessions={activePreviewState.sessions}
            previewDesktopByTabId={activePreviewState.desktopByTabId}
            terminalLabelsById={activeTerminalLabelsById}
            onActivate={activateRightPanelSurface}
            onCloseSurface={closeDisplayedRightPanelSurface}
            onCloseOtherSurfaces={closeOtherDisplayedRightPanelSurfaces}
            onCloseSurfacesToRight={closeDisplayedRightPanelSurfacesToRight}
            onCloseAllSurfaces={closeAllDisplayedRightPanelSurfaces}
            onCopyFilePath={copyRightPanelFilePath}
            onAddBrowser={createBrowserSurface}
            onAddTerminal={addTerminalSurface}
            onAddDiff={addDiffSurface}
            onAddSourceControl={addSourceControlSurface}
            onAddFiles={addFilesSurface}
            onAddDevice={onAddDevice}
            browserAvailable={isPreviewSupportedInRuntime()}
            diffAvailable={gitRightPanelAvailable}
            sourceControlAvailable={gitRightPanelAvailable}
            filesAvailable={activeProject !== null}
          >
            {rightPanelContent}
          </RightPanelTabs>
        </RightPanelSheet>
      ) : null}

      {expandedImage && (
        <ExpandedImageDialog
          key={`${expandedImage.images[expandedImage.index]?.src ?? "image"}:${expandedImage.index}`}
          preview={expandedImage}
          onClose={closeExpandedImage}
        />
      )}
    </div>
  );
}

export default function ChatView(props: ChatViewProps) {
  return (
    <DiffWorkerPoolProvider>
      <ChatViewContent {...props} />
    </DiffWorkerPoolProvider>
  );
}
