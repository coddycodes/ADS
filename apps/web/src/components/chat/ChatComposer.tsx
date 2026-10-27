import type {
  ApprovalRequestId,
  EnvironmentId,
  ModelSelection,
  PreviewAnnotationPayload,
  ProviderApprovalDecision,
  ProviderInteractionMode,
  ResolvedKeybindingsConfig,
  RuntimeMode,
  ScopedThreadRef,
  ServerProvider,
  ThreadId,
  TurnId,
} from "@coddy/contracts";
import { isSteerQueuedMessageShortcut } from "../../keybindings";
import {
  ProviderDriverKind,
  ProviderInstanceId,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
  PROVIDER_SEND_TURN_MAX_ATTACHMENT_BYTES,
} from "@coddy/contracts";
import { type EnvironmentConnectionPresentation } from "@coddy/client-runtime/connection";
import { createModelSelection, normalizeModelSlug } from "@coddy/shared/model";
import {
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  clampCollapsedComposerCursor,
  collapseExpandedComposerCursor,
  type ComposerInlineTokenContext,
  detectComposerTrigger,
  expandCollapsedComposerCursor,
  parseStandaloneComposerCoddyStudioAction,
  type ComposerCoddyStudioAction,
  type ComposerTrigger,
  replaceTextRange,
} from "../../composer-logic";
import { deriveComposerSendState, readFileAsDataUrl } from "../ChatView.logic";
import {
  type ComposerAttachment,
  type ComposerImageAttachment,
  type DraftId,
  useComposerDraftStore,
  useComposerThreadDraft,
  useEffectiveComposerModelState,
} from "../../composerDraftStore";
import {
  type TerminalContextDraft,
  type TerminalContextSelection,
  insertInlineTerminalContextPlaceholder,
  removeInlineTerminalContextPlaceholder,
} from "../../lib/terminalContext";
import { useComposerPathSearch } from "../../lib/composerPathSearchState";
import { useProviderCapabilities } from "../../state/providerCapabilities";
import { type ElementContextDraft } from "../../lib/elementContext";
import { ComposerPendingElementContexts } from "./ComposerPendingElementContexts";
import { ComposerPendingReviewComments } from "./ComposerPendingReviewComments";
import { ComposerPreviewAnnotationCards } from "./ComposerPreviewAnnotationCards";
import {
  shouldUseCompactComposerPrimaryActions,
  shouldUseCompactComposerFooter,
} from "../composerFooterLayout";
import { type ComposerPromptEditorHandle, ComposerPromptEditor } from "../ComposerPromptEditor";
import { ProviderModelPicker } from "./ProviderModelPicker";
import { ComposerCommandMenu } from "./ComposerCommandMenu";
import { buildComposerCommandItems, type ComposerCommandItem } from "./composerCommandItems";
import { deriveComposerCapabilityProfile } from "./composerCapabilities";
import { ComposerPendingApprovalActions } from "./ComposerPendingApprovalActions";
import { ComposerPrimaryActions } from "./ComposerPrimaryActions";
import { ComposerPendingApprovalPanel } from "./ComposerPendingApprovalPanel";
import { ComposerPendingUserInputPanel } from "./ComposerPendingUserInputPanel";
import { ComposerPlanFollowUpBanner } from "./ComposerPlanFollowUpBanner";
import { resolveComposerMenuActiveItemId } from "./composerMenuHighlight";
import {
  getComposerPromptInjectionState,
  getComposerProviderState,
  renderComposerTraitControls,
} from "./composerProviderState";
import { ContextWindowMeter } from "./ContextWindowMeter";
import {
  deriveMcpStatusSnapshot,
  McpStatusPopover,
  type McpStatusSnapshot,
} from "./McpStatusPopover";
import { buildExpandedImagePreview, type ExpandedImagePreview } from "./ExpandedImagePreview";
import { formatMemoryBytes as formatBytes } from "../status-bar/statusBarFormat";
import { cn, randomUUID } from "~/lib/utils";
import { Separator } from "../ui/separator";
import { Button } from "../ui/button";
import { Select, SelectItem, SelectPopup, SelectTrigger } from "../ui/select";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { toastManager } from "../ui/toast";
import {
  CircleAlertIcon,
  FileIcon,
  ListTodoIcon,
  type LucideIcon,
  LockIcon,
  LockOpenIcon,
  MapIcon,
  PenLineIcon,
  PaperclipIcon,
  XIcon,
} from "lucide-react";
import { proposedPlanTitle } from "../../proposedPlan";
import {
  getProviderInteractionModeToggle,
  type ProviderControlAvailability,
} from "../../providerModels";
import {
  applyProviderInstanceSettings,
  deriveProviderInstanceEntries,
  sortProviderInstanceEntries,
  type ProviderInstanceEntry,
} from "../../providerInstances";
import { type AppModelOption, getAppModelOptionsForInstance } from "../../modelSelection";
import type { UnifiedSettings } from "@coddy/contracts/settings";
import type { SessionPhase, Thread } from "../../types";
import type { PendingUserInputDraftAnswer } from "../../pendingUserInput";
import type { PendingApproval, PendingUserInput } from "../../session-logic";
import {
  deriveLatestContextWindowSnapshot,
  formatProviderDisplayName,
} from "../../lib/contextWindow";
import { useMediaQuery } from "../../hooks/useMediaQuery";
import type { ReviewCommentContext } from "../../reviewCommentContext";
import { useThreadHasTerminalSurface } from "../../terminalSurfaceState";

const ATTACHMENT_SIZE_LIMIT_LABEL = `${Math.round(PROVIDER_SEND_TURN_MAX_ATTACHMENT_BYTES / (1024 * 1024))}MB`;

const runtimeModeConfig: Record<
  RuntimeMode,
  { label: string; description: string; icon: LucideIcon }
> = {
  "approval-required": {
    label: "Supervised",
    description: "Ask before commands and file changes.",
    icon: LockIcon,
  },
  "auto-accept-edits": {
    label: "Auto-accept edits",
    description: "Auto-approve edits, ask before other actions.",
    icon: PenLineIcon,
  },
  "full-access": {
    label: "Full access",
    description: "Allow commands and edits without prompts.",
    icon: LockOpenIcon,
  },
};

const runtimeModeOptions = Object.keys(runtimeModeConfig) as RuntimeMode[];
const ACTIVE_CONTROL_CLASSNAME =
  "border-foreground/12 bg-foreground/10 text-foreground hover:border-foreground/18 hover:bg-foreground/15 hover:text-foreground dark:border-foreground/18 dark:bg-foreground/14 dark:hover:border-foreground/24 dark:hover:bg-foreground/20 [&_svg]:!text-foreground";
const COMPOSER_FLOATING_LAYER_SELECTOR = [
  '[data-slot="popover-popup"]',
  '[data-slot="menu-popup"]',
  '[data-slot="select-popup"]',
  '[data-slot="combobox-popup"]',
  '[data-slot="autocomplete-popup"]',
].join(",");

const extendReplacementRangeForTrailingSpace = (
  text: string,
  rangeEnd: number,
  replacement: string,
): number => {
  if (!replacement.endsWith(" ")) {
    return rangeEnd;
  }
  return text[rangeEnd] === " " ? rangeEnd + 1 : rangeEnd;
};

const composerItemMatchesTrigger = (
  item: ComposerCommandItem,
  trigger: ComposerTrigger,
): boolean => {
  if (trigger.kind === "coddy-studio-action") {
    return item.type === "coddy-studio-action";
  }
  if (trigger.kind === "provider-slash") {
    return (
      item.type === "provider-command" ||
      (item.type === "provider-skill" && item.skill.invocation === "slash")
    );
  }
  if (trigger.kind === "provider-dollar-skill") {
    return item.type === "provider-skill" && item.skill.invocation === "dollar";
  }
  return item.type === "file-reference" || item.type === "agent-reference";
};

const syncTerminalContextsByIds = (
  contexts: ReadonlyArray<TerminalContextDraft>,
  ids: ReadonlyArray<string>,
): TerminalContextDraft[] => {
  const contextsById = new Map(contexts.map((context) => [context.id, context]));
  return ids.flatMap((id) => {
    const context = contextsById.get(id);
    return context ? [context] : [];
  });
};

const terminalContextIdListsEqual = (
  contexts: ReadonlyArray<TerminalContextDraft>,
  ids: ReadonlyArray<string>,
): boolean =>
  contexts.length === ids.length && contexts.every((context, index) => context.id === ids[index]);

function isInsideComposerFloatingLayer(element: Element): boolean {
  return element.closest(COMPOSER_FLOATING_LAYER_SELECTOR) !== null;
}

/**
 * Renders the provider trait controls from an element, so their callbacks are props rather than
 * arguments of a render-time call.
 */
function ComposerFooterTraitControls({
  hidden,
  ...input
}: Parameters<typeof renderComposerTraitControls>[0] & { hidden: boolean }) {
  const traitControls = renderComposerTraitControls(input);
  if (hidden || !traitControls) {
    return null;
  }
  return (
    <>
      <Separator orientation="vertical" className="mx-0.5 hidden h-4 sm:block" />
      {traitControls}
    </>
  );
}

const ComposerFooterModeControls = memo(function ComposerFooterModeControls(props: {
  interactionModeAvailability: ProviderControlAvailability;
  interactionMode: ProviderInteractionMode;
  runtimeMode: RuntimeMode;
  showPlanToggle: boolean;
  planSidebarLabel: string;
  planSidebarOpen: boolean;
  onToggleInteractionMode: () => void;
  onRuntimeModeChange: (mode: RuntimeMode) => void;
  onTogglePlanSidebar: () => void;
}) {
  const runtimeModeOption = runtimeModeConfig[props.runtimeMode];
  const RuntimeModeIcon = runtimeModeOption.icon;
  const isFullAccess = props.runtimeMode === "full-access";
  const interactionModeTooltip =
    props.interactionMode === "plan" ? "Disable plan mode" : "Enable plan mode";
  const planSidebarTooltip = props.planSidebarOpen
    ? `Hide ${props.planSidebarLabel.toLowerCase()} sidebar`
    : `Show ${props.planSidebarLabel.toLowerCase()} sidebar`;

  const interactionModeToggle = (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            variant="ghost"
            className={cn(
              "shrink-0 px-2",
              props.interactionModeAvailability.state === "supported"
                ? props.interactionMode === "plan"
                  ? ACTIVE_CONTROL_CLASSNAME
                  : "text-muted-foreground hover:text-foreground/80"
                : "border border-input bg-background text-muted-foreground",
            )}
            size="sm"
            type="button"
            onClick={() => {
              if (props.interactionModeAvailability.state !== "supported") return;
              props.onToggleInteractionMode();
            }}
            aria-label={
              props.interactionModeAvailability.state === "supported"
                ? interactionModeTooltip
                : props.interactionModeAvailability.reason
            }
            aria-pressed={props.interactionMode === "plan"}
            aria-disabled={props.interactionModeAvailability.state !== "supported"}
          />
        }
      >
        <MapIcon className="size-4" />
      </TooltipTrigger>
      <TooltipPopup side="top">
        {props.interactionModeAvailability.state === "supported"
          ? interactionModeTooltip
          : props.interactionModeAvailability.reason}
      </TooltipPopup>
    </Tooltip>
  );

  return (
    <>
      <Separator orientation="vertical" className="mx-0.5 hidden h-4 sm:block" />

      <Tooltip>
        <Select
          value={props.runtimeMode}
          onValueChange={(value) => props.onRuntimeModeChange(value!)}
        >
          <TooltipTrigger
            render={
              <SelectTrigger
                variant="ghost"
                size="sm"
                className={cn(
                  "shrink-0 px-2 text-foreground/80 hover:text-foreground [&_[data-slot=select-icon]]:hidden",
                  isFullAccess ? "[&_svg]:text-destructive" : "[&_svg]:text-foreground/80",
                )}
                aria-label={runtimeModeOption.label}
              />
            }
          >
            <RuntimeModeIcon className="size-4" />
          </TooltipTrigger>
          <SelectPopup alignItemWithTrigger={false}>
            {runtimeModeOptions.map((mode) => {
              const option = runtimeModeConfig[mode];
              const OptionIcon = option.icon;
              return (
                <SelectItem key={mode} value={mode} className="min-w-64 py-2">
                  <div className="grid min-w-0 gap-0.5">
                    <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
                      <OptionIcon
                        className={cn(
                          "size-3.5 shrink-0",
                          isFullAccess && mode === "full-access"
                            ? "text-destructive"
                            : "text-muted-foreground",
                        )}
                      />
                      {option.label}
                    </span>
                    <span className="text-muted-foreground text-xs leading-4">
                      {option.description}
                    </span>
                  </div>
                </SelectItem>
              );
            })}
          </SelectPopup>
        </Select>
        <TooltipPopup side="top">{runtimeModeOption.description}</TooltipPopup>
      </Tooltip>

      {interactionModeToggle}

      {props.showPlanToggle ? (
        <>
          <Separator orientation="vertical" className="mx-0.5 hidden h-4 sm:block" />
          <Tooltip>
            <TooltipTrigger
              render={
                <Button
                  variant="ghost"
                  className={cn(
                    "shrink-0 whitespace-nowrap px-2 sm:px-3",
                    props.planSidebarOpen
                      ? ACTIVE_CONTROL_CLASSNAME
                      : "text-muted-foreground hover:text-foreground/80",
                  )}
                  size="sm"
                  type="button"
                  onClick={props.onTogglePlanSidebar}
                  aria-label={planSidebarTooltip}
                />
              }
            >
              <ListTodoIcon
                className={props.planSidebarOpen ? "text-current opacity-100" : undefined}
              />
              <span className="sr-only sm:not-sr-only">{props.planSidebarLabel}</span>
            </TooltipTrigger>
            <TooltipPopup side="top">{planSidebarTooltip}</TooltipPopup>
          </Tooltip>
        </>
      ) : null}
    </>
  );
});

const ComposerFooterPrimaryActions = memo(function ComposerFooterPrimaryActions(props: {
  compact: boolean;
  activeContextWindow: ReturnType<typeof deriveLatestContextWindowSnapshot>;
  activeMcpStatus: McpStatusSnapshot;
  activeThreadProviderDisplayName: string | null;
  supportsMcpStatus: boolean;
  supportsContextWindowUsage: boolean;
  showContextWindowMeter: boolean;
  isPreparingWorktree: boolean;
  pendingAction: {
    questionIndex: number;
    isLastQuestion: boolean;
    canAdvance: boolean;
    isResponding: boolean;
    isComplete: boolean;
  } | null;
  isRunning: boolean;
  canCancelPendingSend: boolean;
  showPlanFollowUpPrompt: boolean;
  promptHasText: boolean;
  isSendBusy: boolean;
  isConnecting: boolean;
  isEnvironmentUnavailable: boolean;
  sendBlockedReason: string | null;
  hasSendableContent: boolean;
  isAttachmentSelectionDisabled: boolean;
  preserveComposerFocusOnPointerDown?: boolean;
  onSelectAttachments: () => void;
  onPreviousPendingQuestion: () => void;
  onInterrupt: () => void;
  onImplementPlanInNewThread: () => void;
}) {
  return (
    <>
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              type="button"
              aria-label="Attach files"
              disabled={props.isAttachmentSelectionDisabled}
              onClick={props.onSelectAttachments}
            />
          }
        >
          <PaperclipIcon />
        </TooltipTrigger>
        <TooltipPopup side="top">Attach files</TooltipPopup>
      </Tooltip>
      {props.isPreparingWorktree ? (
        <span className="text-muted-foreground text-xs">Preparing worktree...</span>
      ) : null}
      <McpStatusPopover supported={props.supportsMcpStatus} snapshot={props.activeMcpStatus} />
      {props.showContextWindowMeter ? (
        <ContextWindowMeter
          supported={props.supportsContextWindowUsage}
          usage={props.activeContextWindow}
          providerDisplayName={props.activeThreadProviderDisplayName}
        />
      ) : null}
      <ComposerPrimaryActions
        compact={props.compact}
        pendingAction={props.pendingAction}
        isRunning={props.isRunning}
        canCancelPendingSend={props.canCancelPendingSend}
        showPlanFollowUpPrompt={props.showPlanFollowUpPrompt}
        promptHasText={props.promptHasText}
        isSendBusy={props.isSendBusy}
        isConnecting={props.isConnecting}
        isEnvironmentUnavailable={props.isEnvironmentUnavailable}
        sendBlockedReason={props.sendBlockedReason}
        isPreparingWorktree={props.isPreparingWorktree}
        hasSendableContent={props.hasSendableContent}
        preserveComposerFocusOnPointerDown={props.preserveComposerFocusOnPointerDown ?? false}
        onPreviousPendingQuestion={props.onPreviousPendingQuestion}
        onInterrupt={props.onInterrupt}
        onImplementPlanInNewThread={props.onImplementPlanInNewThread}
      />
    </>
  );
});

// --------------------------------------------------------------------------
// Handle exposed to ChatView
// --------------------------------------------------------------------------

export interface ChatComposerHandle {
  focusAtEnd: () => void;
  focusAt: (cursor: number) => void;
  insertTextAtEnd: (text: string) => boolean;
  openModelPicker: () => void;
  toggleModelPicker: () => void;
  isModelPickerOpen: () => boolean;
  readSnapshot: () => {
    value: string;
    cursor: number;
    expandedCursor: number;
    terminalContextIds: string[];
  };
  /** Reset composer cursor/trigger/highlight after external prompt mutations (e.g. onSend). */
  resetCursorState: (options?: {
    cursor?: number;
    prompt?: string;
    detectTrigger?: boolean;
  }) => void;
  /** Insert a terminal context from a supported terminal surface. */
  addTerminalContext: (selection: TerminalContextSelection) => boolean;
  /** Get the current prompt/effort/model state for use in send. */
  getSendContext: () => {
    prompt: string;
    attachments: ComposerAttachment[];
    terminalContexts: TerminalContextDraft[];
    elementContexts: ElementContextDraft[];
    previewAnnotations: PreviewAnnotationPayload[];
    reviewComments: ReviewCommentContext[];
    selectedPromptEffort: string | null;
    selectedModelOptionsForDispatch: unknown;
    selectedModelSelection: ModelSelection;
    selectedProvider: ProviderDriverKind;
    selectedModel: string;
    selectedProviderModels: ReadonlyArray<ServerProvider["models"][number]>;
  };
}

// --------------------------------------------------------------------------
// Props
// --------------------------------------------------------------------------

export interface ChatComposerProps {
  composerDraftTarget: ScopedThreadRef | DraftId;
  environmentId: EnvironmentId;
  routeKind: "server" | "draft";
  routeThreadRef: ScopedThreadRef;
  draftId: DraftId | null;

  // Thread context
  activeThreadId: ThreadId | null;
  activeThreadEnvironmentId: EnvironmentId | undefined;
  activeThread: Thread | undefined;
  isServerThread: boolean;
  isLocalDraftThread: boolean;

  // Session phase
  phase: SessionPhase;
  isConnecting: boolean;
  isSendBusy: boolean;
  canCancelPendingSend?: boolean;
  isPreparingWorktree: boolean;
  environmentUnavailable: {
    readonly label: string;
    readonly connection: EnvironmentConnectionPresentation;
  } | null;

  // Pending approvals / inputs
  activePendingApproval: PendingApproval | null;
  pendingApprovals: PendingApproval[];
  pendingUserInputs: PendingUserInput[];
  activePendingProgress: {
    questionIndex: number;
    isLastQuestion: boolean;
    canAdvance: boolean;
    customAnswer: string;
    activeQuestion: { id: string; multiSelect?: boolean | undefined } | null;
  } | null;
  activePendingResolvedAnswers: Record<string, unknown> | null;
  activePendingIsResponding: boolean;
  activePendingDraftAnswers: Record<string, PendingUserInputDraftAnswer>;
  activePendingQuestionIndex: number;
  respondingRequestIds: ApprovalRequestId[];

  // Plan
  showPlanFollowUpPrompt: boolean;
  activeProposedPlan: Thread["proposedPlans"][number] | null;
  activePlan: { turnId?: TurnId } | null;
  sidebarProposedPlan: { turnId?: TurnId } | null;
  planSidebarLabel: string;
  planSidebarOpen: boolean;

  // Mode
  runtimeMode: RuntimeMode;
  interactionMode: ProviderInteractionMode;

  // Provider / model
  lockedProvider: ProviderDriverKind | null;
  /** Authoritative routing key resolved by the host from thread/session/provider state. */
  providerBindingInstanceId: ProviderInstanceId;
  /** Restricts provider/model navigation to the active instance for panel or exact-lock chats. */
  lockProviderPickerToActiveInstance: boolean;
  /** Blocks composition while session and live provider metadata disagree. */
  providerBindingConflictReason: string | null;
  providerStatuses: ServerProvider[];
  activeProjectDefaultModelSelection: ModelSelection | null | undefined;
  activeThreadModelSelection: ModelSelection | null | undefined;
  onCommitModelSelection?: (selection: ModelSelection) => Promise<void>;

  // Context window
  activeThreadActivities: Thread["activities"] | undefined;

  // Misc
  resolvedTheme: "light" | "dark";
  settings: UnifiedSettings;
  keybindings: ResolvedKeybindingsConfig;
  gitCwd: string | null;

  // Refs the parent needs kept in sync
  promptRef: React.RefObject<string>;
  composerAttachmentsRef: React.RefObject<ComposerAttachment[]>;
  composerTerminalContextsRef: React.RefObject<TerminalContextDraft[]>;
  composerElementContextsRef: React.RefObject<ElementContextDraft[]>;
  composerRef: React.RefObject<ChatComposerHandle | null>;

  // Callbacks
  onSend: (e?: { preventDefault: () => void }) => void;
  onInterrupt: () => void;
  onSteerQueuedMessage?: () => void;
  onImplementPlanInNewThread: () => void;
  onRespondToApproval: (
    requestId: ApprovalRequestId,
    decision: ProviderApprovalDecision,
  ) => Promise<unknown>;
  onSelectActivePendingUserInputOption: (questionId: string, optionLabel: string) => void;
  onAdvanceActivePendingUserInput: () => void;
  onPreviousActivePendingUserInputQuestion: () => void;
  onChangeActivePendingUserInputCustomAnswer: (
    questionId: string,
    value: string,
    nextCursor: number,
    expandedCursor: number,
    cursorAdjacentToMention: boolean,
  ) => void;

  onProviderModelSelect: (instanceId: ProviderInstanceId, model: string) => void;
  getModelDisabledReason: (instanceId: ProviderInstanceId, model: string) => string | null;
  toggleInteractionMode: () => void;
  handleRuntimeModeChange: (mode: RuntimeMode) => void;
  handleInteractionModeChange: (mode: ProviderInteractionMode) => void;
  togglePlanSidebar: () => void;

  focusComposer: () => void;
  scheduleComposerFocus: () => void;
  setThreadError: (threadId: ThreadId | null, error: string | null) => void;
  onExpandImage: (preview: ExpandedImagePreview) => void;
}

// --------------------------------------------------------------------------
// Component
// --------------------------------------------------------------------------

export const ChatComposer = memo(function ChatComposer(props: ChatComposerProps) {
  const {
    composerDraftTarget,
    environmentId,
    routeKind,
    routeThreadRef,
    draftId,
    activeThreadId,
    activeThreadEnvironmentId: _activeThreadEnvironmentId,
    activeThread,
    isServerThread: _isServerThread,
    isLocalDraftThread: _isLocalDraftThread,
    phase,
    isConnecting,
    isSendBusy,
    canCancelPendingSend = false,
    isPreparingWorktree,
    environmentUnavailable,
    activePendingApproval,
    pendingApprovals,
    pendingUserInputs,
    activePendingProgress,
    activePendingResolvedAnswers,
    activePendingIsResponding,
    activePendingDraftAnswers,
    activePendingQuestionIndex,
    respondingRequestIds,
    showPlanFollowUpPrompt,
    activeProposedPlan,
    activePlan,
    sidebarProposedPlan,
    planSidebarLabel,
    planSidebarOpen,
    runtimeMode,
    interactionMode,
    lockedProvider,
    providerBindingInstanceId,
    lockProviderPickerToActiveInstance,
    providerBindingConflictReason,
    providerStatuses,
    activeProjectDefaultModelSelection,
    activeThreadModelSelection,
    onCommitModelSelection,
    activeThreadActivities,
    resolvedTheme,
    settings,
    keybindings,
    gitCwd,
    promptRef,
    composerRef,
    composerAttachmentsRef,
    composerTerminalContextsRef,
    composerElementContextsRef,
    onSend,
    onInterrupt,
    onSteerQueuedMessage,
    onImplementPlanInNewThread,
    onRespondToApproval,
    onSelectActivePendingUserInputOption,
    onAdvanceActivePendingUserInput,
    onPreviousActivePendingUserInputQuestion,
    onChangeActivePendingUserInputCustomAnswer,
    onProviderModelSelect,
    getModelDisabledReason,
    toggleInteractionMode,
    handleRuntimeModeChange,
    handleInteractionModeChange,
    togglePlanSidebar,
    focusComposer,
    scheduleComposerFocus,
    setThreadError,
    onExpandImage,
  } = props;
  const isProviderBindingConflicted = providerBindingConflictReason !== null;
  const terminalOpen = useThreadHasTerminalSurface(routeThreadRef);

  // ------------------------------------------------------------------
  // Store subscriptions (prompt / attachments / terminal contexts)
  // ------------------------------------------------------------------
  const composerDraft = useComposerThreadDraft(composerDraftTarget);
  const prompt = composerDraft.prompt;
  const composerAttachments = composerDraft.attachments;
  const composerImages = composerAttachments.filter(
    (attachment): attachment is ComposerImageAttachment => attachment.type === "image",
  );
  const composerTerminalContexts = composerDraft.terminalContexts;
  const composerElementContexts = composerDraft.elementContexts;
  const composerPreviewAnnotations = composerDraft.previewAnnotations;
  const composerReviewComments = composerDraft.reviewComments;
  const nonPersistedComposerAttachmentIds = composerDraft.nonPersistedAttachmentIds;

  const setComposerDraftPrompt = useComposerDraftStore((store) => store.setPrompt);
  const addComposerDraftAttachments = useComposerDraftStore((store) => store.addAttachments);
  const removeComposerDraftAttachment = useComposerDraftStore((store) => store.removeAttachment);
  const insertComposerDraftTerminalContext = useComposerDraftStore(
    (store) => store.insertTerminalContext,
  );
  const removeComposerDraftTerminalContext = useComposerDraftStore(
    (store) => store.removeTerminalContext,
  );
  const setComposerDraftTerminalContexts = useComposerDraftStore(
    (store) => store.setTerminalContexts,
  );
  const removeComposerDraftElementContext = useComposerDraftStore(
    (store) => store.removeElementContext,
  );
  const removeComposerDraftPreviewAnnotation = useComposerDraftStore(
    (store) => store.removePreviewAnnotation,
  );
  const removeComposerDraftReviewComment = useComposerDraftStore(
    (store) => store.removeReviewComment,
  );
  const clearComposerDraftPersistedAttachments = useComposerDraftStore(
    (store) => store.clearPersistedAttachments,
  );
  const syncComposerDraftPersistedAttachments = useComposerDraftStore(
    (store) => store.syncPersistedAttachments,
  );
  const getComposerDraft = useComposerDraftStore((store) => store.getComposerDraft);
  const setComposerDraftProviderModelOptions = useComposerDraftStore(
    (store) => store.setProviderModelOptions,
  );

  // ------------------------------------------------------------------
  // Model state
  // ------------------------------------------------------------------
  // Instance-aware projection of the wire provider list. One entry per
  // configured instance (default built-in + any custom `providerInstances.*`),
  // sorted default-first per driver kind for a stable picker order.
  const providerInstanceEntries = useMemo<ReadonlyArray<ProviderInstanceEntry>>(
    () =>
      sortProviderInstanceEntries(
        applyProviderInstanceSettings(deriveProviderInstanceEntries(providerStatuses), settings),
      ),
    [providerStatuses, settings],
  );
  const selectedProviderByThreadId = composerDraft.activeProvider ?? null;
  const hasExplicitSelectedInstanceId = Boolean(
    selectedProviderByThreadId ??
    activeThread?.session?.providerInstanceId ??
    activeThreadModelSelection?.instanceId ??
    activeProjectDefaultModelSelection?.instanceId,
  );
  const unlockedSelectedProvider =
    providerInstanceEntries.find((entry) => entry.instanceId === providerBindingInstanceId)
      ?.driverKind ?? ProviderDriverKind.make("codex");
  const selectedProvider: ProviderDriverKind = lockedProvider ?? unlockedSelectedProvider;
  const lockedContinuationGroupKey = useMemo((): string | null => {
    if (!lockedProvider || !activeThread) return null;
    return (
      providerInstanceEntries.find((entry) => entry.instanceId === providerBindingInstanceId)
        ?.continuationGroupKey ?? null
    );
  }, [activeThread, lockedProvider, providerBindingInstanceId, providerInstanceEntries]);
  const selectedInstanceId = providerBindingInstanceId;

  const { modelOptions: composerModelOptions, selectedModel } = useEffectiveComposerModelState({
    threadRef: composerDraftTarget,
    providers: providerStatuses,
    selectedProvider,
    selectedInstanceId,
    threadModelSelection: activeThreadModelSelection,
    projectModelSelection: activeProjectDefaultModelSelection,
    settings,
  });

  // Resolve the active instance's snapshot by `instanceId` so a custom
  // instance gets its own model list and readiness state.
  const selectedProviderEntry = useMemo(
    () => providerInstanceEntries.find((entry) => entry.instanceId === selectedInstanceId),
    [providerInstanceEntries, selectedInstanceId],
  );
  const selectedProviderStatus = useMemo(
    () => selectedProviderEntry?.snapshot ?? null,
    [selectedProviderEntry],
  );
  const providerCapabilities = useProviderCapabilities({
    environmentId,
    instanceId: selectedInstanceId,
    cwd: gitCwd,
  });
  const nativeSkillInvocation =
    selectedProviderEntry?.driverKind === "codex"
      ? "dollar"
      : selectedProviderEntry &&
          ["claudeAgent", "cursor", "opencode"].includes(selectedProviderEntry.driverKind)
        ? "slash"
        : undefined;
  const composerCapabilities = useMemo(
    () => deriveComposerCapabilityProfile(providerCapabilities.data, nativeSkillInvocation),
    [providerCapabilities.data, nativeSkillInvocation],
  );
  const composerInlineTokenContext = useMemo<ComposerInlineTokenContext>(
    () => ({
      mentionableAgentNames: composerCapabilities.mentionableAgentNames,
      enabledDollarSkillNames: new Set(
        composerCapabilities.dollarSkills.map((skill) => skill.name),
      ),
    }),
    [composerCapabilities],
  );
  const selectedProviderModels = useMemo<ReadonlyArray<ServerProvider["models"][number]>>(
    () => selectedProviderEntry?.models ?? [],
    [selectedProviderEntry],
  );

  const composerPromptInjectionState = useMemo(
    () => getComposerPromptInjectionState(prompt),
    [prompt],
  );
  const composerProviderState = useMemo(
    () =>
      getComposerProviderState({
        provider: selectedProvider,
        model: selectedModel,
        models: selectedProviderModels,
        promptInjectionState: composerPromptInjectionState,
        modelOptions: composerModelOptions?.[selectedInstanceId],
      }),
    [
      composerModelOptions,
      composerPromptInjectionState,
      selectedInstanceId,
      selectedModel,
      selectedProvider,
      selectedProviderModels,
    ],
  );

  const selectedPromptEffort = composerProviderState.promptEffort;
  const selectedModelOptionsForDispatch = composerProviderState.modelOptionsForDispatch;
  const composerProviderControls = useMemo(
    () => ({
      interactionModeAvailability: getProviderInteractionModeToggle(
        providerStatuses,
        selectedProvider,
        selectedInstanceId,
        hasExplicitSelectedInstanceId,
      ),
    }),
    [hasExplicitSelectedInstanceId, providerStatuses, selectedInstanceId, selectedProvider],
  );
  const selectedModelSelection = useMemo<ModelSelection>(
    () => createModelSelection(selectedInstanceId, selectedModel, selectedModelOptionsForDispatch),
    [selectedInstanceId, selectedModel, selectedModelOptionsForDispatch],
  );
  const mountedRef = useRef(true);
  const modelOptionCommitKey = `${selectedProvider}:${selectedInstanceId}:${selectedModel}`;
  const latestModelOptionCommitRef = useRef(modelOptionCommitKey);
  useLayoutEffect(() => {
    latestModelOptionCommitRef.current = modelOptionCommitKey;
  }, [modelOptionCommitKey]);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  const commitComposerModelOptions = useCallback(
    async (nextOptions: ModelSelection["options"]) => {
      if (isProviderBindingConflicted) return;
      const commitKey = `${selectedProvider}:${selectedInstanceId}:${selectedModel}`;
      const selection = createModelSelection(selectedInstanceId, selectedModel, nextOptions);
      if (routeKind === "server") {
        await onCommitModelSelection?.(selection);
      }
      if (!mountedRef.current || latestModelOptionCommitRef.current !== commitKey) return;
      setComposerDraftProviderModelOptions(composerDraftTarget, selectedProvider, nextOptions, {
        instanceId: selectedInstanceId,
        model: selectedModel,
        persistSticky: true,
      });
    },
    [
      composerDraftTarget,
      isProviderBindingConflicted,
      onCommitModelSelection,
      routeKind,
      selectedInstanceId,
      selectedModel,
      selectedProvider,
      setComposerDraftProviderModelOptions,
    ],
  );
  const selectedModelForPicker = selectedModel;
  // Instance-keyed option list so the picker can show each configured
  // instance (built-in + custom) in the unified model list. The
  // options are server-reported models plus that exact instance's
  // configured custom models; selected slugs are not injected into lists.
  const modelOptionsByInstance = useMemo<
    ReadonlyMap<ProviderInstanceId, ReadonlyArray<AppModelOption>>
  >(() => {
    const out = new Map<ProviderInstanceId, ReadonlyArray<AppModelOption>>();
    for (const entry of providerInstanceEntries) {
      out.set(entry.instanceId, getAppModelOptionsForInstance(settings, entry));
    }
    return out;
  }, [providerInstanceEntries, settings]);
  const selectedModelForPickerWithCustomFallback = useMemo(() => {
    const currentOptions = modelOptionsByInstance.get(selectedInstanceId) ?? [];
    return currentOptions.some((option) => option.slug === selectedModelForPicker)
      ? selectedModelForPicker
      : (normalizeModelSlug(selectedModelForPicker, selectedProvider) ?? selectedModelForPicker);
  }, [modelOptionsByInstance, selectedInstanceId, selectedModelForPicker, selectedProvider]);

  // ------------------------------------------------------------------
  // Context window
  // ------------------------------------------------------------------
  const activeContextWindow = useMemo(
    () => deriveLatestContextWindowSnapshot(activeThreadActivities ?? []),
    [activeThreadActivities],
  );
  const supportsContextWindowUsage = selectedProviderStatus?.supportsContextWindowUsage === true;
  const supportsMcpStatus = selectedProviderStatus?.supportsMcpStatus === true;
  const activeMcpStatus = useMemo(
    () =>
      deriveMcpStatusSnapshot(
        activeThreadActivities ?? [],
        selectedInstanceId,
        phase === "ready" || phase === "running",
      ),
    [activeThreadActivities, phase, selectedInstanceId],
  );
  const activeThreadProviderDisplayName = useMemo(() => {
    if (!activeThreadModelSelection) return null;
    const entry = providerInstanceEntries.find(
      (entry) => entry.instanceId === activeThreadModelSelection.instanceId,
    );
    if (entry) {
      return entry.displayName;
    }
    return formatProviderDisplayName(activeThreadModelSelection.instanceId);
  }, [providerInstanceEntries, activeThreadModelSelection]);

  // ------------------------------------------------------------------
  // Composer-local state
  // ------------------------------------------------------------------
  const [composerCursor, setComposerCursor] = useState(() =>
    collapseExpandedComposerCursor(prompt, prompt.length, composerInlineTokenContext),
  );
  const [composerTrigger, setComposerTrigger] = useState<ComposerTrigger | null>(() =>
    detectComposerTrigger(prompt, prompt.length, composerCapabilities.trigger),
  );
  const [composerHighlightedItemId, setComposerHighlightedItemId] = useState<string | null>(null);
  const [composerHighlightedSearchKey, setComposerHighlightedSearchKey] = useState<string | null>(
    null,
  );
  const [isDragOverComposer, setIsDragOverComposer] = useState(false);
  const [isComposerFooterCompact, setIsComposerFooterCompact] = useState(false);
  const [isComposerPrimaryActionsCompact, setIsComposerPrimaryActionsCompact] = useState(false);
  const [isComposerModelPickerOpen, setIsComposerModelPickerOpen] = useState(false);
  const [isComposerFocused, setIsComposerFocused] = useState(false);
  const isMobileViewport = useMediaQuery("max-sm");
  const isComposerCollapsedMobile = isMobileViewport && !isComposerFocused;

  // ------------------------------------------------------------------
  // Refs
  // ------------------------------------------------------------------
  const composerEditorRef = useRef<ComposerPromptEditorHandle>(null);
  const previousComposerCursorMappingRef = useRef({
    prompt,
    inlineTokenContext: composerInlineTokenContext,
  });
  const composerFormRef = useRef<HTMLFormElement>(null);
  const composerFileInputRef = useRef<HTMLInputElement>(null);
  const composerSurfaceRef = useRef<HTMLDivElement>(null);
  const composerSelectLockRef = useRef(false);
  const composerMenuOpenRef = useRef(false);
  const composerMenuItemsRef = useRef<ReadonlyArray<ComposerCommandItem>>([]);
  const activeComposerMenuItemRef = useRef<ComposerCommandItem | null>(null);
  const composerBlurFrameRef = useRef<number | null>(null);
  const mobileComposerExpandFrameRef = useRef<number | null>(null);
  const mobileComposerExpandReleaseFrameRef = useRef<number | null>(null);
  const mobileComposerExpandInFlightRef = useRef(false);

  // ------------------------------------------------------------------
  // Reset compositor state on thread/draft change
  // ------------------------------------------------------------------
  const [composerResetScope, setComposerResetScope] = useState(() => ({
    draftId,
    activeThreadId,
  }));
  const isComposerResetPending =
    composerResetScope.draftId !== draftId || composerResetScope.activeThreadId !== activeThreadId;
  if (isComposerResetPending) {
    setComposerResetScope({ draftId, activeThreadId });
    setComposerHighlightedItemId(null);
    setComposerCursor(
      collapseExpandedComposerCursor(prompt, prompt.length, composerInlineTokenContext),
    );
    setComposerTrigger(detectComposerTrigger(prompt, prompt.length, composerCapabilities.trigger));
    setIsDragOverComposer(false);
  }
  // Drag depth counts per thread/draft, so a switch mid-drag starts again from zero.
  const dragDepthRef = useRef({ scope: composerResetScope, depth: 0 });
  const readDragDepth = () =>
    dragDepthRef.current.scope === composerResetScope ? dragDepthRef.current.depth : 0;
  const writeDragDepth = (depth: number) => {
    dragDepthRef.current = { scope: composerResetScope, depth };
  };

  // ------------------------------------------------------------------
  // Derived: composer send state
  // ------------------------------------------------------------------
  const composerSendState = useMemo(
    () =>
      deriveComposerSendState({
        prompt,
        imageCount: composerAttachments.length,
        terminalContexts: composerTerminalContexts,
        elementContextCount:
          composerElementContexts.length +
          composerPreviewAnnotations.length +
          composerReviewComments.length,
      }),
    [
      composerElementContexts.length,
      composerAttachments.length,
      composerPreviewAnnotations.length,
      composerReviewComments.length,
      composerTerminalContexts,
      prompt,
    ],
  );

  // ------------------------------------------------------------------
  // Derived: composer trigger / menu
  // ------------------------------------------------------------------
  const composerTriggerKind = composerTrigger?.kind ?? null;
  const pathTriggerQuery =
    composerTrigger?.kind === "provider-reference" ? composerTrigger.query : "";
  const isPathTrigger = composerTriggerKind === "provider-reference";
  const workspaceEntries = useComposerPathSearch({
    environmentId,
    cwd: isPathTrigger ? gitCwd : null,
    query: isPathTrigger ? pathTriggerQuery : null,
  });

  const composerMenuResult = useMemo(
    () =>
      buildComposerCommandItems({
        trigger: composerTrigger,
        providerInstanceId: selectedInstanceId,
        capabilities: composerCapabilities,
        pathSearch: workspaceEntries,
      }),
    [composerCapabilities, composerTrigger, selectedInstanceId, workspaceEntries],
  );
  const composerMenuItems = composerMenuResult.items;

  const composerMenuOpen = Boolean(composerTrigger);
  const { revalidateIfStale: revalidateProviderCapabilitiesIfStale } = providerCapabilities;
  useEffect(() => {
    if (composerMenuOpen) revalidateProviderCapabilitiesIfStale();
  }, [composerMenuOpen, revalidateProviderCapabilitiesIfStale]);
  const composerMenuSearchKey = composerTrigger
    ? `${composerTrigger.kind}:${composerTrigger.query.trim().toLowerCase()}`
    : null;
  const activeComposerMenuItem = useMemo(() => {
    const activeItemId = resolveComposerMenuActiveItemId({
      items: composerMenuItems,
      highlightedItemId: composerHighlightedItemId,
      currentSearchKey: composerMenuSearchKey,
      highlightedSearchKey: composerHighlightedSearchKey,
      preferredItemId: composerMenuResult.preferredItemId,
    });
    return composerMenuItems.find((item) => item.id === activeItemId) ?? null;
  }, [
    composerHighlightedItemId,
    composerHighlightedSearchKey,
    composerMenuItems,
    composerMenuResult.preferredItemId,
    composerMenuSearchKey,
  ]);

  useLayoutEffect(() => {
    composerMenuOpenRef.current = composerMenuOpen;
    composerMenuItemsRef.current = composerMenuItems;
    activeComposerMenuItemRef.current = activeComposerMenuItem;
  }, [activeComposerMenuItem, composerMenuItems, composerMenuOpen]);

  const nonPersistedComposerAttachmentIdSet = useMemo(
    () => new Set(nonPersistedComposerAttachmentIds),
    [nonPersistedComposerAttachmentIds],
  );

  const isComposerApprovalState = activePendingApproval !== null;
  const activePendingUserInput = pendingUserInputs[0] ?? null;
  const hasComposerHeader =
    isComposerApprovalState ||
    pendingUserInputs.length > 0 ||
    (showPlanFollowUpPrompt && activeProposedPlan !== null);
  const showCollapsedMobilePromptRow =
    isComposerCollapsedMobile && !isComposerApprovalState && pendingUserInputs.length === 0;

  const composerFooterHasWideActions = showPlanFollowUpPrompt || activePendingProgress !== null;
  const showPlanSidebarToggle = Boolean(activePlan || sidebarProposedPlan || planSidebarOpen);

  const isProviderCapabilityMenu =
    composerTriggerKind === "provider-dollar-skill" ||
    composerTriggerKind === "provider-slash" ||
    composerTriggerKind === "provider-reference";
  const isComposerMenuLoading =
    (isProviderCapabilityMenu && providerCapabilities.isPending) ||
    (composerTriggerKind === "provider-reference" &&
      pathTriggerQuery.length > 0 &&
      workspaceEntries.isPending);
  const capabilityIssue = isProviderCapabilityMenu
    ? (providerCapabilities.error ?? providerCapabilities.data?.issues[0] ?? null)
    : null;

  // ------------------------------------------------------------------
  // Provider traits UI
  // ------------------------------------------------------------------
  const setPromptFromTraits = useCallback(
    (nextPrompt: string) => {
      if (isProviderBindingConflicted) return;
      if (nextPrompt === promptRef.current) {
        scheduleComposerFocus();
        return;
      }
      promptRef.current = nextPrompt;
      setComposerDraftPrompt(composerDraftTarget, nextPrompt);
      const nextCursor = collapseExpandedComposerCursor(
        nextPrompt,
        nextPrompt.length,
        composerInlineTokenContext,
      );
      setComposerCursor(nextCursor);
      setComposerTrigger(
        detectComposerTrigger(nextPrompt, nextPrompt.length, composerCapabilities.trigger),
      );
      scheduleComposerFocus();
    },
    [
      composerCapabilities,
      composerDraftTarget,
      composerInlineTokenContext,
      isProviderBindingConflicted,
      promptRef,
      scheduleComposerFocus,
      setComposerDraftPrompt,
    ],
  );

  const composerTraitControls = (
    <ComposerFooterTraitControls
      hidden={isProviderBindingConflicted}
      provider={selectedProvider}
      instanceId={selectedInstanceId}
      {...(routeKind === "server" ? { threadRef: routeThreadRef } : {})}
      {...(routeKind === "draft" && draftId ? { draftId } : {})}
      model={selectedModel}
      models={selectedProviderModels}
      providerSnapshotLoaded={selectedProviderStatus !== null}
      modelOptions={composerModelOptions?.[selectedInstanceId]}
      prompt={prompt}
      onPromptChange={setPromptFromTraits}
      onModelOptionsChange={commitComposerModelOptions}
    />
  );
  const pendingPrimaryAction = useMemo(
    () =>
      activePendingProgress
        ? {
            questionIndex: activePendingProgress.questionIndex,
            isLastQuestion: activePendingProgress.isLastQuestion,
            canAdvance: activePendingProgress.canAdvance,
            isResponding: activePendingIsResponding,
            isComplete: Boolean(activePendingResolvedAnswers),
          }
        : null,
    [activePendingIsResponding, activePendingProgress, activePendingResolvedAnswers],
  );
  const collapsedComposerPrimaryActionDisabled =
    isProviderBindingConflicted ||
    phase === "running" ||
    isSendBusy ||
    isConnecting ||
    !composerSendState.hasSendableContent;
  const collapsedComposerPrimaryActionLabel = "Send message";
  const showMobilePendingAnswerActions =
    isMobileViewport && !isComposerCollapsedMobile && pendingPrimaryAction !== null;

  // ------------------------------------------------------------------
  // Prompt helpers
  // ------------------------------------------------------------------
  const setPrompt = useCallback(
    (nextPrompt: string) => {
      if (isProviderBindingConflicted) return;
      setComposerDraftPrompt(composerDraftTarget, nextPrompt);
    },
    [composerDraftTarget, isProviderBindingConflicted, setComposerDraftPrompt],
  );

  const addComposerAttachmentsToDraft = useCallback(
    (attachments: ComposerAttachment[]) => {
      return addComposerDraftAttachments(composerDraftTarget, attachments, {
        maxAttachments: PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
      });
    },
    [composerDraftTarget, addComposerDraftAttachments],
  );

  const removeComposerAttachmentFromDraft = useCallback(
    (attachmentId: string) => {
      if (isProviderBindingConflicted) return;
      removeComposerDraftAttachment(composerDraftTarget, attachmentId);
    },
    [composerDraftTarget, isProviderBindingConflicted, removeComposerDraftAttachment],
  );

  const removeComposerTerminalContextFromDraft = useCallback(
    (contextId: string) => {
      if (isProviderBindingConflicted) return;
      const contextIndex = composerTerminalContexts.findIndex(
        (context) => context.id === contextId,
      );
      if (contextIndex < 0) return;
      const removal = removeInlineTerminalContextPlaceholder(promptRef.current, contextIndex);
      promptRef.current = removal.prompt;
      setPrompt(removal.prompt);
      removeComposerDraftTerminalContext(composerDraftTarget, contextId);
      const nextCursor = collapseExpandedComposerCursor(
        removal.prompt,
        removal.cursor,
        composerInlineTokenContext,
      );
      setComposerCursor(nextCursor);
      setComposerTrigger(
        detectComposerTrigger(removal.prompt, removal.cursor, composerCapabilities.trigger),
      );
    },
    [
      composerDraftTarget,
      composerCapabilities,
      composerInlineTokenContext,
      composerTerminalContexts,
      isProviderBindingConflicted,
      promptRef,
      removeComposerDraftTerminalContext,
      setPrompt,
    ],
  );

  // ------------------------------------------------------------------
  // Sync refs back to parent
  // ------------------------------------------------------------------
  useEffect(() => {
    promptRef.current = prompt;
    const previousMapping = previousComposerCursorMappingRef.current;
    previousComposerCursorMappingRef.current = {
      prompt,
      inlineTokenContext: composerInlineTokenContext,
    };
    setComposerCursor((existing) => {
      if (
        previousMapping.prompt === prompt &&
        previousMapping.inlineTokenContext !== composerInlineTokenContext
      ) {
        const expandedCursor = expandCollapsedComposerCursor(
          prompt,
          existing,
          previousMapping.inlineTokenContext,
        );
        return collapseExpandedComposerCursor(prompt, expandedCursor, composerInlineTokenContext);
      }
      return clampCollapsedComposerCursor(prompt, existing, composerInlineTokenContext);
    });
  }, [composerInlineTokenContext, prompt, promptRef]);

  useEffect(() => {
    composerAttachmentsRef.current = composerAttachments;
  }, [composerAttachments, composerAttachmentsRef]);

  useEffect(() => {
    composerTerminalContextsRef.current = composerTerminalContexts;
  }, [composerTerminalContexts, composerTerminalContextsRef]);

  useEffect(() => {
    composerElementContextsRef.current = composerElementContexts;
  }, [composerElementContexts, composerElementContextsRef]);

  // ------------------------------------------------------------------
  // Composer menu highlight sync
  // ------------------------------------------------------------------
  // The highlight follows the resolved active item; a closed menu has neither (its search key is
  // null too). A pending thread/draft reset re-renders first, so this pass must not override it.
  const syncedComposerHighlightedItemId = composerMenuOpen
    ? (activeComposerMenuItem?.id ?? null)
    : null;
  if (!isComposerResetPending) {
    if (composerHighlightedItemId !== syncedComposerHighlightedItemId) {
      setComposerHighlightedItemId(syncedComposerHighlightedItemId);
    }
    if (composerHighlightedSearchKey !== composerMenuSearchKey) {
      setComposerHighlightedSearchKey(composerMenuSearchKey);
    }
  }

  const lastSyncedPendingInputRef = useRef<{
    requestId: string | null;
    questionId: string | null;
  } | null>(null);

  useEffect(() => {
    const nextCustomAnswer = activePendingProgress?.customAnswer;
    if (typeof nextCustomAnswer !== "string") {
      lastSyncedPendingInputRef.current = null;
      return;
    }

    const nextRequestId = activePendingUserInput?.requestId ?? null;
    const nextQuestionId = activePendingProgress?.activeQuestion?.id ?? null;
    const questionChanged =
      lastSyncedPendingInputRef.current?.requestId !== nextRequestId ||
      lastSyncedPendingInputRef.current?.questionId !== nextQuestionId;
    const textChangedExternally = promptRef.current !== nextCustomAnswer;

    lastSyncedPendingInputRef.current = {
      requestId: nextRequestId,
      questionId: nextQuestionId,
    };

    if (!questionChanged && !textChangedExternally) {
      return;
    }

    promptRef.current = nextCustomAnswer;
    const nextCursor = collapseExpandedComposerCursor(
      nextCustomAnswer,
      nextCustomAnswer.length,
      composerInlineTokenContext,
    );
    setComposerCursor(nextCursor);
    setComposerTrigger(
      detectComposerTrigger(
        nextCustomAnswer,
        expandCollapsedComposerCursor(nextCustomAnswer, nextCursor, composerInlineTokenContext),
        composerCapabilities.trigger,
      ),
    );
    setComposerHighlightedItemId(null);
  }, [
    activePendingProgress?.customAnswer,
    activePendingProgress?.activeQuestion?.id,
    activePendingUserInput?.requestId,
    composerCapabilities,
    composerInlineTokenContext,
    promptRef,
  ]);

  // ------------------------------------------------------------------
  // Footer compact layout observation
  // ------------------------------------------------------------------
  useLayoutEffect(() => {
    const composerForm = composerFormRef.current;
    if (!composerForm) return;
    const measureComposerFormWidth = () => composerForm.clientWidth;
    const measureFooterCompactness = () => {
      const composerFormWidth = measureComposerFormWidth();
      const footerCompact = shouldUseCompactComposerFooter(composerFormWidth, {
        hasWideActions: composerFooterHasWideActions,
      });
      const primaryActionsCompact =
        footerCompact &&
        shouldUseCompactComposerPrimaryActions(composerFormWidth, {
          hasWideActions: composerFooterHasWideActions,
        });
      return {
        primaryActionsCompact,
        footerCompact,
      };
    };

    const initialCompactness = measureFooterCompactness();
    setIsComposerPrimaryActionsCompact(initialCompactness.primaryActionsCompact);
    setIsComposerFooterCompact(initialCompactness.footerCompact);
    if (typeof ResizeObserver === "undefined") return;

    const observer = new ResizeObserver(() => {
      const nextCompactness = measureFooterCompactness();
      setIsComposerPrimaryActionsCompact((previous) =>
        previous === nextCompactness.primaryActionsCompact
          ? previous
          : nextCompactness.primaryActionsCompact,
      );
      setIsComposerFooterCompact((previous) =>
        previous === nextCompactness.footerCompact ? previous : nextCompactness.footerCompact,
      );
    });

    observer.observe(composerForm);
    return () => {
      observer.disconnect();
    };
  }, [composerFooterHasWideActions]);

  // ------------------------------------------------------------------
  // Attachment persist effect
  // ------------------------------------------------------------------
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (composerAttachments.length === 0) {
        clearComposerDraftPersistedAttachments(composerDraftTarget);
        return;
      }
      const getPersistedAttachmentsForThread = () =>
        getComposerDraft(composerDraftTarget)?.persistedAttachments ?? [];
      try {
        const currentPersistedAttachments = getPersistedAttachmentsForThread();
        const existingPersistedById = new Map(
          currentPersistedAttachments.map((attachment) => [attachment.id, attachment]),
        );
        const serialized = (
          await Promise.all(
            composerAttachments.map(async (attachment) => {
              try {
                const dataUrl = await readFileAsDataUrl(attachment.file);
                return {
                  type: attachment.type,
                  id: attachment.id,
                  name: attachment.name,
                  mimeType: attachment.mimeType,
                  sizeBytes: attachment.sizeBytes,
                  dataUrl,
                };
              } catch {
                return existingPersistedById.get(attachment.id) ?? null;
              }
            }),
          )
        ).flatMap((attachment) => (attachment ? [attachment] : []));
        if (cancelled) return;
        syncComposerDraftPersistedAttachments(composerDraftTarget, serialized);
      } catch {
        const currentAttachmentIds = new Set(
          composerAttachments.map((attachment) => attachment.id),
        );
        const fallbackPersistedAttachments = getPersistedAttachmentsForThread();
        const fallbackPersistedIds: Array<string> = [];
        for (const attachment of fallbackPersistedAttachments) {
          if (currentAttachmentIds.has(attachment.id)) {
            fallbackPersistedIds.push(attachment.id);
          }
        }
        const fallbackPersistedIdSet = new Set(fallbackPersistedIds);
        const fallbackAttachments = fallbackPersistedAttachments.filter((attachment) =>
          fallbackPersistedIdSet.has(attachment.id),
        );
        if (cancelled) return;
        syncComposerDraftPersistedAttachments(composerDraftTarget, fallbackAttachments);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    composerDraftTarget,
    clearComposerDraftPersistedAttachments,
    composerAttachments,
    getComposerDraft,
    syncComposerDraftPersistedAttachments,
  ]);

  // ------------------------------------------------------------------
  // Callbacks: prompt change
  // ------------------------------------------------------------------
  const onPromptChange = useCallback(
    (
      nextPrompt: string,
      nextCursor: number,
      expandedCursor: number,
      cursorAdjacentToMention: boolean,
      terminalContextIds: string[],
    ) => {
      if (isProviderBindingConflicted) return;
      const activePendingQuestion = activePendingProgress?.activeQuestion;
      if (activePendingQuestion && pendingUserInputs.length > 0) {
        setComposerCursor(nextCursor);
        setComposerTrigger(
          cursorAdjacentToMention
            ? null
            : detectComposerTrigger(nextPrompt, expandedCursor, composerCapabilities.trigger),
        );
        onChangeActivePendingUserInputCustomAnswer(
          activePendingQuestion.id,
          nextPrompt,
          nextCursor,
          expandedCursor,
          cursorAdjacentToMention,
        );
        return;
      }
      promptRef.current = nextPrompt;
      setPrompt(nextPrompt);
      if (!terminalContextIdListsEqual(composerTerminalContexts, terminalContextIds)) {
        setComposerDraftTerminalContexts(
          composerDraftTarget,
          syncTerminalContextsByIds(composerTerminalContexts, terminalContextIds),
        );
      }
      setComposerCursor(nextCursor);
      setComposerTrigger(
        cursorAdjacentToMention
          ? null
          : detectComposerTrigger(nextPrompt, expandedCursor, composerCapabilities.trigger),
      );
    },
    [
      activePendingProgress?.activeQuestion,
      composerCapabilities.trigger,
      pendingUserInputs.length,
      onChangeActivePendingUserInputCustomAnswer,
      promptRef,
      setPrompt,
      composerDraftTarget,
      composerTerminalContexts,
      isProviderBindingConflicted,
      setComposerDraftTerminalContexts,
    ],
  );

  // ------------------------------------------------------------------
  // Callbacks: prompt replacement / menu
  // ------------------------------------------------------------------
  const applyPromptReplacement = useCallback(
    (
      rangeStart: number,
      rangeEnd: number,
      replacement: string,
      options?: { expectedText?: string; focusEditorAfterReplace?: boolean },
    ): boolean => {
      if (isProviderBindingConflicted) return false;
      const currentText = promptRef.current;
      const safeStart = Math.max(0, Math.min(currentText.length, rangeStart));
      const safeEnd = Math.max(safeStart, Math.min(currentText.length, rangeEnd));
      if (
        options?.expectedText !== undefined &&
        currentText.slice(safeStart, safeEnd) !== options.expectedText
      ) {
        return false;
      }
      const next = replaceTextRange(promptRef.current, rangeStart, rangeEnd, replacement);
      const nextCursor = collapseExpandedComposerCursor(
        next.text,
        next.cursor,
        composerInlineTokenContext,
      );
      const nextExpandedCursor = expandCollapsedComposerCursor(
        next.text,
        nextCursor,
        composerInlineTokenContext,
      );
      promptRef.current = next.text;
      const activePendingQuestion = activePendingProgress?.activeQuestion;
      if (activePendingQuestion && activePendingUserInput) {
        onChangeActivePendingUserInputCustomAnswer(
          activePendingQuestion.id,
          next.text,
          nextCursor,
          nextExpandedCursor,
          false,
        );
      } else {
        setPrompt(next.text);
      }
      setComposerCursor(nextCursor);
      setComposerTrigger(
        detectComposerTrigger(next.text, nextExpandedCursor, composerCapabilities.trigger),
      );
      if (options?.focusEditorAfterReplace !== false) {
        window.requestAnimationFrame(() => {
          composerEditorRef.current?.focusAt(nextCursor);
        });
      }
      return true;
    },
    [
      activePendingProgress?.activeQuestion,
      activePendingUserInput,
      composerCapabilities,
      composerInlineTokenContext,
      isProviderBindingConflicted,
      onChangeActivePendingUserInputCustomAnswer,
      promptRef,
      setPrompt,
    ],
  );

  const readComposerSnapshot = useCallback((): {
    value: string;
    cursor: number;
    expandedCursor: number;
    terminalContextIds: string[];
  } => {
    const editorSnapshot = composerEditorRef.current?.readSnapshot();
    if (editorSnapshot) {
      return editorSnapshot;
    }
    return {
      value: promptRef.current,
      cursor: composerCursor,
      expandedCursor: expandCollapsedComposerCursor(
        promptRef.current,
        composerCursor,
        composerInlineTokenContext,
      ),
      terminalContextIds: composerTerminalContexts.map((context) => context.id),
    };
  }, [composerInlineTokenContext, composerCursor, composerTerminalContexts, promptRef]);

  // Re-detect only when the provider's supported trigger surface changes. A cleared trigger
  // closes the menu, and the highlight sync clears its highlight on that render.
  const detectedTriggerSurfaceRef = useRef<{
    signature: string;
    instanceId: ProviderInstanceId | undefined;
  } | null>(null);
  useLayoutEffect(() => {
    const signature = composerCapabilities.signature;
    const instanceId = selectedProviderStatus?.instanceId;
    const detectedSurface = detectedTriggerSurfaceRef.current;
    if (detectedSurface?.signature === signature && detectedSurface.instanceId === instanceId) {
      return;
    }
    detectedTriggerSurfaceRef.current = { signature, instanceId };
    const snapshot = readComposerSnapshot();
    setComposerTrigger(
      detectComposerTrigger(snapshot.value, snapshot.expandedCursor, composerCapabilities.trigger),
    );
  }, [
    composerCapabilities.signature,
    composerCapabilities.trigger,
    readComposerSnapshot,
    selectedProviderStatus?.instanceId,
  ]);

  const executeCoddyStudioAction = useCallback(
    (action: ComposerCoddyStudioAction) => {
      if (action === "model") {
        setIsComposerModelPickerOpen(true);
        return;
      }
      void handleInteractionModeChange(action);
    },
    [handleInteractionModeChange],
  );

  const resolveActiveComposerTrigger = useCallback((): {
    snapshot: { value: string; cursor: number; expandedCursor: number };
    trigger: ComposerTrigger | null;
  } => {
    const snapshot = readComposerSnapshot();
    return {
      snapshot,
      trigger: detectComposerTrigger(
        snapshot.value,
        snapshot.expandedCursor,
        composerCapabilities.trigger,
      ),
    };
  }, [composerCapabilities.trigger, readComposerSnapshot]);

  const onSelectComposerItem = useCallback(
    (selectedItem: ComposerCommandItem) => {
      if (composerSelectLockRef.current) return;
      const item = composerMenuItemsRef.current.find(
        (currentItem) => currentItem.id === selectedItem.id,
      );
      if (!item) return;
      composerSelectLockRef.current = true;
      window.requestAnimationFrame(() => {
        composerSelectLockRef.current = false;
      });
      const { snapshot, trigger } = resolveActiveComposerTrigger();
      if (!trigger || !composerItemMatchesTrigger(item, trigger)) return;
      if (item.type === "coddy-studio-action") {
        const applied = applyPromptReplacement(trigger.rangeStart, trigger.rangeEnd, "", {
          expectedText: snapshot.value.slice(trigger.rangeStart, trigger.rangeEnd),
        });
        if (applied) {
          setComposerHighlightedItemId(null);
          setComposerHighlightedSearchKey(null);
          executeCoddyStudioAction(item.action);
        }
        return;
      }

      const replacementRangeEnd = extendReplacementRangeForTrailingSpace(
        snapshot.value,
        trigger.rangeEnd,
        item.replacement,
      );
      const applied = applyPromptReplacement(
        trigger.rangeStart,
        replacementRangeEnd,
        item.replacement,
        { expectedText: snapshot.value.slice(trigger.rangeStart, replacementRangeEnd) },
      );
      if (applied) {
        setComposerHighlightedItemId(null);
        setComposerHighlightedSearchKey(null);
      }
    },
    [applyPromptReplacement, executeCoddyStudioAction, resolveActiveComposerTrigger],
  );

  const onComposerMenuItemHighlighted = useCallback(
    (itemId: string | null) => {
      setComposerHighlightedItemId(itemId);
      setComposerHighlightedSearchKey(composerMenuSearchKey);
    },
    [composerMenuSearchKey],
  );

  const nudgeComposerMenuHighlight = useCallback(
    (key: "ArrowDown" | "ArrowUp") => {
      if (composerMenuItems.length === 0) return;
      const highlightedIndex = composerMenuItems.findIndex(
        (item) => item.id === composerHighlightedItemId,
      );
      const normalizedIndex =
        highlightedIndex >= 0 ? highlightedIndex : key === "ArrowDown" ? -1 : 0;
      const offset = key === "ArrowDown" ? 1 : -1;
      const nextIndex =
        (normalizedIndex + offset + composerMenuItems.length) % composerMenuItems.length;
      const nextItem = composerMenuItems[nextIndex];
      setComposerHighlightedItemId(nextItem?.id ?? null);
    },
    [composerHighlightedItemId, composerMenuItems],
  );

  const blurMobileComposerAfterSend = useCallback(() => {
    if (!isMobileViewport) return;
    if (composerBlurFrameRef.current !== null) {
      window.cancelAnimationFrame(composerBlurFrameRef.current);
      composerBlurFrameRef.current = null;
    }
    const activeElement = document.activeElement;
    if (activeElement instanceof HTMLElement) {
      activeElement.blur();
    }
    setIsComposerFocused(false);
  }, [isMobileViewport]);

  const shouldBlurMobileComposerOnSubmit = useCallback(() => {
    if (!isMobileViewport) return false;
    if (isSendBusy || isConnecting || phase === "running") return false;
    if (activePendingProgress) {
      return activePendingProgress.isLastQuestion && Boolean(activePendingResolvedAnswers);
    }
    return showPlanFollowUpPrompt || composerSendState.hasSendableContent;
  }, [
    activePendingProgress,
    activePendingResolvedAnswers,
    composerSendState.hasSendableContent,
    isConnecting,
    isMobileViewport,
    isSendBusy,
    phase,
    showPlanFollowUpPrompt,
  ]);

  const submitComposer = useCallback(
    (event?: { preventDefault: () => void }) => {
      if (isProviderBindingConflicted) {
        event?.preventDefault();
        return;
      }
      const currentPrompt = promptRef.current;
      const coddyStudioAction = parseStandaloneComposerCoddyStudioAction(currentPrompt);
      if (coddyStudioAction) {
        event?.preventDefault();
        const applied = applyPromptReplacement(0, currentPrompt.length, "", {
          expectedText: currentPrompt,
        });
        if (!applied) {
          return;
        }
        setComposerHighlightedItemId(null);
        setComposerHighlightedSearchKey(null);
        executeCoddyStudioAction(coddyStudioAction);
        return;
      }
      onSend(event);
      if (shouldBlurMobileComposerOnSubmit()) {
        blurMobileComposerAfterSend();
      }
    },
    [
      applyPromptReplacement,
      blurMobileComposerAfterSend,
      executeCoddyStudioAction,
      isProviderBindingConflicted,
      onSend,
      promptRef,
      shouldBlurMobileComposerOnSubmit,
    ],
  );
  const expandMobileComposer = useCallback(() => {
    if (composerBlurFrameRef.current !== null) {
      window.cancelAnimationFrame(composerBlurFrameRef.current);
      composerBlurFrameRef.current = null;
    }
    if (mobileComposerExpandFrameRef.current !== null) {
      window.cancelAnimationFrame(mobileComposerExpandFrameRef.current);
    }
    if (mobileComposerExpandReleaseFrameRef.current !== null) {
      window.cancelAnimationFrame(mobileComposerExpandReleaseFrameRef.current);
    }
    mobileComposerExpandInFlightRef.current = true;
    setIsComposerFocused(true);
    mobileComposerExpandFrameRef.current = window.requestAnimationFrame(() => {
      mobileComposerExpandFrameRef.current = null;
      composerEditorRef.current?.focusAtEnd();
      mobileComposerExpandReleaseFrameRef.current = window.requestAnimationFrame(() => {
        mobileComposerExpandReleaseFrameRef.current = null;
        mobileComposerExpandInFlightRef.current = false;
      });
    });
  }, []);

  // ------------------------------------------------------------------
  // Callbacks: command key
  // ------------------------------------------------------------------
  const onComposerCommandKey = (
    key: "ArrowDown" | "ArrowUp" | "Enter" | "Tab",
    event: KeyboardEvent,
  ) => {
    if (isSteerQueuedMessageShortcut(event, keybindings, { context: { editableFocus: true } })) {
      event.preventDefault();
      if (!isProviderBindingConflicted) onSteerQueuedMessage?.();
      return true;
    }
    if (isProviderBindingConflicted) return true;
    if (key === "Tab" && event.shiftKey) {
      toggleInteractionMode();
      return true;
    }
    const { trigger } = resolveActiveComposerTrigger();
    const menuIsActive = composerMenuOpenRef.current || trigger !== null;
    if (menuIsActive) {
      const currentItems = composerMenuItemsRef.current;
      const selectedItem = activeComposerMenuItemRef.current ?? currentItems[0];
      if (key === "ArrowDown" && currentItems.length > 0) {
        nudgeComposerMenuHighlight("ArrowDown");
        return true;
      }
      if (key === "ArrowUp" && currentItems.length > 0) {
        nudgeComposerMenuHighlight("ArrowUp");
        return true;
      }
      if ((key === "Enter" || key === "Tab") && selectedItem) {
        onSelectComposerItem(selectedItem);
        return true;
      }
    }
    if (key === "Enter" && !event.shiftKey) {
      submitComposer();
      return true;
    }
    return false;
  };

  // ------------------------------------------------------------------
  // Callbacks: attachments
  // ------------------------------------------------------------------
  const addComposerAttachments = (files: File[]) => {
    if (isProviderBindingConflicted) return;
    if (!activeThreadId || files.length === 0) return;
    if (isComposerApprovalState || pendingUserInputs.length > 0) {
      toastManager.add({
        type: "error",
        title: "Attach files after answering plan questions.",
      });
      return;
    }
    const nextAttachments: ComposerAttachment[] = [];
    const attachmentInputIndexes = new Map<string, number>();
    const rejections: Array<{ index: number; message: string }> = [];
    const reject = (index: number, message: string) => {
      rejections.push({ index, message });
    };
    for (const [index, file] of files.entries()) {
      if (file.size === 0) {
        reject(index, `'${file.name || "file"}' is empty and cannot be attached.`);
        continue;
      }
      if (file.size > PROVIDER_SEND_TURN_MAX_ATTACHMENT_BYTES) {
        reject(
          index,
          `'${file.name || "file"}' exceeds the ${ATTACHMENT_SIZE_LIMIT_LABEL} attachment limit.`,
        );
        continue;
      }
      const mimeType = file.type || "application/octet-stream";
      const baseAttachment = {
        id: randomUUID(),
        name: file.name || "file",
        mimeType,
        sizeBytes: file.size,
        file,
      };
      const attachment: ComposerAttachment = mimeType.startsWith("image/")
        ? { ...baseAttachment, type: "image", previewUrl: URL.createObjectURL(file) }
        : { ...baseAttachment, type: "file" };
      nextAttachments.push(attachment);
      attachmentInputIndexes.set(attachment.id, index);
    }
    if (nextAttachments.length > 0) {
      const { rejectedCapacityAttachments } = addComposerAttachmentsToDraft(nextAttachments);
      for (const attachment of rejectedCapacityAttachments) {
        reject(
          attachmentInputIndexes.get(attachment.id) ?? Number.MAX_SAFE_INTEGER,
          `'${attachment.name}' cannot be attached: you can attach up to ${PROVIDER_SEND_TURN_MAX_ATTACHMENTS} files per message.`,
        );
      }
    }
    const firstRejection = rejections.sort((left, right) => left.index - right.index)[0];
    setThreadError(activeThreadId, firstRejection?.message ?? null);
  };

  const removeComposerAttachment = (attachmentId: string) => {
    removeComposerAttachmentFromDraft(attachmentId);
  };

  const onComposerFileInputChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    if (!isProviderBindingConflicted) {
      addComposerAttachments(Array.from(event.currentTarget.files ?? []));
    }
    event.currentTarget.value = "";
  };

  // ------------------------------------------------------------------
  // Callbacks: paste / drag
  // ------------------------------------------------------------------
  const onComposerPaste = (event: React.ClipboardEvent<HTMLElement>) => {
    const files = Array.from(event.clipboardData.files);
    if (files.length === 0) return;
    event.preventDefault();
    if (isProviderBindingConflicted) return;
    addComposerAttachments(files);
  };

  const onComposerDragEnter = (event: React.DragEvent<HTMLDivElement>) => {
    if (!event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    if (isProviderBindingConflicted) return;
    writeDragDepth(readDragDepth() + 1);
    setIsDragOverComposer(true);
  };

  const onComposerDragOver = (event: React.DragEvent<HTMLDivElement>) => {
    if (!event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    if (isProviderBindingConflicted) {
      event.dataTransfer.dropEffect = "none";
      return;
    }
    event.dataTransfer.dropEffect = "copy";
    setIsDragOverComposer(true);
  };

  const onComposerDragLeave = (event: React.DragEvent<HTMLDivElement>) => {
    if (!event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    if (isProviderBindingConflicted) return;
    const nextTarget = event.relatedTarget;
    if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) return;
    const nextDragDepth = Math.max(0, readDragDepth() - 1);
    writeDragDepth(nextDragDepth);
    if (nextDragDepth === 0) {
      setIsDragOverComposer(false);
    }
  };

  const onComposerDrop = (event: React.DragEvent<HTMLDivElement>) => {
    if (!event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    writeDragDepth(0);
    setIsDragOverComposer(false);
    if (isProviderBindingConflicted) return;
    const files = Array.from(event.dataTransfer.files);
    addComposerAttachments(files);
    focusComposer();
  };
  const handleInterruptPrimaryAction = useCallback(() => {
    void onInterrupt();
  }, [onInterrupt]);
  const handleImplementPlanInNewThreadPrimaryAction = useCallback(() => {
    void onImplementPlanInNewThread();
  }, [onImplementPlanInNewThread]);
  const scheduleComposerCollapseCheck = useCallback(() => {
    if (!isMobileViewport) {
      return;
    }
    if (mobileComposerExpandInFlightRef.current) {
      return;
    }
    if (composerBlurFrameRef.current !== null) {
      window.cancelAnimationFrame(composerBlurFrameRef.current);
    }
    composerBlurFrameRef.current = window.requestAnimationFrame(() => {
      composerBlurFrameRef.current = null;
      if (mobileComposerExpandInFlightRef.current) {
        return;
      }
      const composerSurface = composerSurfaceRef.current;
      const activeElement = document.activeElement;
      if (activeElement instanceof Element && isInsideComposerFloatingLayer(activeElement)) {
        return;
      }
      if (
        composerSurface &&
        activeElement instanceof Node &&
        composerSurface.contains(activeElement)
      ) {
        return;
      }
      setIsComposerFocused(false);
    });
  }, [isMobileViewport]);

  useEffect(() => {
    return () => {
      if (composerBlurFrameRef.current !== null) {
        window.cancelAnimationFrame(composerBlurFrameRef.current);
      }
      if (mobileComposerExpandFrameRef.current !== null) {
        window.cancelAnimationFrame(mobileComposerExpandFrameRef.current);
      }
      if (mobileComposerExpandReleaseFrameRef.current !== null) {
        window.cancelAnimationFrame(mobileComposerExpandReleaseFrameRef.current);
      }
    };
  }, []);

  // ------------------------------------------------------------------
  // Imperative handle
  // ------------------------------------------------------------------
  useImperativeHandle(
    composerRef,
    () => ({
      focusAtEnd: () => {
        composerEditorRef.current?.focusAtEnd();
      },
      focusAt: (cursor: number) => {
        composerEditorRef.current?.focusAt(cursor);
      },
      insertTextAtEnd: (text: string) => {
        if (
          text.length === 0 ||
          isConnecting ||
          isProviderBindingConflicted ||
          isComposerApprovalState ||
          pendingUserInputs.length > 0 ||
          (environmentUnavailable !== null && activePendingProgress === null)
        ) {
          return false;
        }
        const rangeEnd = promptRef.current.length;
        return applyPromptReplacement(rangeEnd, rangeEnd, text);
      },
      openModelPicker: () => {
        if (isProviderBindingConflicted) return;
        setIsComposerModelPickerOpen(true);
      },
      toggleModelPicker: () => {
        if (isProviderBindingConflicted) return;
        setIsComposerModelPickerOpen((open) => !open);
      },
      isModelPickerOpen: () => isComposerModelPickerOpen,
      readSnapshot: () => {
        return readComposerSnapshot();
      },
      resetCursorState: (options?: {
        cursor?: number;
        prompt?: string;
        detectTrigger?: boolean;
      }) => {
        const promptForState = options?.prompt ?? promptRef.current;
        const cursor = clampCollapsedComposerCursor(
          promptForState,
          options?.cursor ?? 0,
          composerInlineTokenContext,
        );
        setComposerHighlightedItemId(null);
        setComposerHighlightedSearchKey(null);
        setComposerCursor(cursor);
        setComposerTrigger(
          options?.detectTrigger
            ? detectComposerTrigger(
                promptForState,
                expandCollapsedComposerCursor(promptForState, cursor, composerInlineTokenContext),
                composerCapabilities.trigger,
              )
            : null,
        );
      },
      addTerminalContext: (selection: TerminalContextSelection) => {
        if (isProviderBindingConflicted || !activeThread) return false;
        const snapshot = composerEditorRef.current?.readSnapshot() ?? {
          value: promptRef.current,
          cursor: composerCursor,
          expandedCursor: expandCollapsedComposerCursor(
            promptRef.current,
            composerCursor,
            composerInlineTokenContext,
          ),
          terminalContextIds: composerTerminalContexts.map((context) => context.id),
        };
        const insertion = insertInlineTerminalContextPlaceholder(
          snapshot.value,
          snapshot.expandedCursor,
        );
        const nextCollapsedCursor = collapseExpandedComposerCursor(
          insertion.prompt,
          insertion.cursor,
          composerInlineTokenContext,
        );
        const inserted = insertComposerDraftTerminalContext(
          composerDraftTarget,
          insertion.prompt,
          {
            id: randomUUID(),
            threadId: activeThread.id,
            createdAt: new Date().toISOString(),
            ...selection,
          },
          insertion.contextIndex,
        );
        if (!inserted) return false;
        promptRef.current = insertion.prompt;
        setComposerCursor(nextCollapsedCursor);
        setComposerTrigger(
          detectComposerTrigger(insertion.prompt, insertion.cursor, composerCapabilities.trigger),
        );
        window.requestAnimationFrame(() => {
          composerEditorRef.current?.focusAt(nextCollapsedCursor);
        });
        return true;
      },
      getSendContext: () => ({
        prompt: promptRef.current,
        attachments: composerAttachmentsRef.current,
        terminalContexts: composerTerminalContextsRef.current,
        elementContexts: composerElementContextsRef.current,
        previewAnnotations: composerPreviewAnnotations,
        reviewComments: composerReviewComments,
        selectedPromptEffort,
        selectedModelOptionsForDispatch,
        selectedModelSelection,
        selectedProvider,
        selectedModel,
        selectedProviderModels,
      }),
    }),
    [
      activeThread,
      composerCapabilities,
      composerInlineTokenContext,
      composerDraftTarget,
      composerCursor,
      composerTerminalContexts,
      insertComposerDraftTerminalContext,
      promptRef,
      composerAttachmentsRef,
      composerTerminalContextsRef,
      composerElementContextsRef,
      composerPreviewAnnotations,
      composerReviewComments,
      isConnecting,
      isComposerApprovalState,
      isProviderBindingConflicted,
      pendingUserInputs.length,
      environmentUnavailable,
      activePendingProgress,
      applyPromptReplacement,
      isComposerModelPickerOpen,
      readComposerSnapshot,
      selectedModel,
      selectedModelOptionsForDispatch,
      selectedModelSelection,
      selectedPromptEffort,
      selectedProvider,
      selectedProviderModels,
    ],
  );

  // Render
  // ------------------------------------------------------------------
  return (
    <form
      ref={composerFormRef}
      onSubmit={submitComposer}
      className="mx-auto w-full min-w-0 max-w-3xl"
      data-chat-composer-form="true"
    >
      <input
        ref={composerFileInputRef}
        type="file"
        multiple
        hidden
        disabled={
          isProviderBindingConflicted || isComposerApprovalState || pendingUserInputs.length > 0
        }
        onChange={onComposerFileInputChange}
      />
      <div
        className={cn(
          "group rounded-[22px] p-px transition-colors duration-200",
          composerProviderState.composerFrameClassName,
        )}
        onDragEnter={onComposerDragEnter}
        onDragOver={onComposerDragOver}
        onDragLeave={onComposerDragLeave}
        onDrop={onComposerDrop}
      >
        <div
          ref={composerSurfaceRef}
          data-chat-composer-mobile-collapsed={isComposerCollapsedMobile ? "true" : "false"}
          className={cn(
            "chat-composer-glass rounded-[20px] border transition-colors duration-200",
            isDragOverComposer ? "border-primary/70 bg-accent/45" : "border-border",
            environmentUnavailable ? "opacity-75" : null,
            composerProviderState.composerSurfaceClassName,
          )}
          onFocusCapture={(event) => {
            const activeElement = event.target;
            if (
              isComposerCollapsedMobile &&
              activeElement instanceof HTMLElement &&
              activeElement.closest('[data-chat-composer-collapsed-controls="true"]')
            ) {
              return;
            }
            if (composerBlurFrameRef.current !== null) {
              window.cancelAnimationFrame(composerBlurFrameRef.current);
              composerBlurFrameRef.current = null;
            }
            setIsComposerFocused(true);
          }}
          onBlurCapture={() => {
            scheduleComposerCollapseCheck();
          }}
        >
          {!isComposerCollapsedMobile &&
            (activePendingApproval ? (
              <div className="rounded-t-[19px] border-b border-border/65 bg-muted/20">
                <ComposerPendingApprovalPanel
                  approval={activePendingApproval}
                  pendingCount={pendingApprovals.length}
                />
              </div>
            ) : pendingUserInputs.length > 0 ? (
              <div className="rounded-t-[19px] border-b border-border/65 bg-muted/20">
                <ComposerPendingUserInputPanel
                  pendingUserInputs={pendingUserInputs}
                  respondingRequestIds={respondingRequestIds}
                  answers={activePendingDraftAnswers}
                  questionIndex={activePendingQuestionIndex}
                  onToggleOption={onSelectActivePendingUserInputOption}
                  onAdvance={onAdvanceActivePendingUserInput}
                />
              </div>
            ) : showPlanFollowUpPrompt && activeProposedPlan ? (
              <div className="rounded-t-[19px] border-b border-border/65 bg-muted/20">
                <ComposerPlanFollowUpBanner
                  key={activeProposedPlan.id}
                  planTitle={proposedPlanTitle(activeProposedPlan.planMarkdown) ?? null}
                />
              </div>
            ) : null)}

          {isComposerCollapsedMobile && activePendingApproval ? (
            <div
              className="rounded-t-[19px] border-b border-border/65 bg-muted/20"
              data-chat-composer-collapsed-controls="true"
            >
              <ComposerPendingApprovalPanel
                approval={activePendingApproval}
                pendingCount={pendingApprovals.length}
              />
              <div className="flex flex-wrap items-center justify-end gap-2 px-3 pb-3 sm:px-4">
                <ComposerPendingApprovalActions
                  requestId={activePendingApproval.requestId}
                  isResponding={respondingRequestIds.includes(activePendingApproval.requestId)}
                  onRespondToApproval={onRespondToApproval}
                />
              </div>
            </div>
          ) : isComposerCollapsedMobile && pendingUserInputs.length > 0 ? (
            <div
              className="rounded-t-[19px] border-b border-border/65 bg-muted/20"
              data-chat-composer-collapsed-controls="true"
            >
              <ComposerPendingUserInputPanel
                pendingUserInputs={pendingUserInputs}
                respondingRequestIds={respondingRequestIds}
                answers={activePendingDraftAnswers}
                questionIndex={activePendingQuestionIndex}
                onToggleOption={onSelectActivePendingUserInputOption}
                onAdvance={onAdvanceActivePendingUserInput}
              />
              <div className="px-3 pb-3 sm:px-4">
                <div
                  data-chat-composer-mobile-pending-compact="true"
                  className={cn(
                    "flex min-w-0 items-center gap-2 rounded-lg border border-border/55 bg-background/55 p-1.5 pl-3 transition-colors hover:bg-background/80",
                    !activePendingProgress?.activeQuestion?.multiSelect && "p-0",
                  )}
                >
                  <button
                    type="button"
                    className={cn(
                      "min-w-0 flex-1 truncate bg-transparent py-1.5 text-left text-sm",
                      activePendingProgress?.customAnswer
                        ? "text-foreground"
                        : "text-muted-foreground",
                      !activePendingProgress?.activeQuestion?.multiSelect && "px-3 py-2",
                    )}
                    onPointerDown={(event) => event.preventDefault()}
                    onClick={expandMobileComposer}
                    aria-label="Write custom answer"
                  >
                    {activePendingProgress?.customAnswer || "Write custom answer"}
                  </button>
                  {activePendingProgress?.activeQuestion?.multiSelect ? (
                    <ComposerPrimaryActions
                      compact
                      pendingAction={pendingPrimaryAction}
                      isRunning={false}
                      showPlanFollowUpPrompt={false}
                      promptHasText={false}
                      isSendBusy={isSendBusy}
                      isConnecting={isConnecting}
                      isEnvironmentUnavailable={environmentUnavailable !== null}
                      sendBlockedReason={providerBindingConflictReason}
                      isPreparingWorktree={false}
                      hasSendableContent={false}
                      preserveComposerFocusOnPointerDown
                      onPreviousPendingQuestion={onPreviousActivePendingUserInputQuestion}
                      onInterrupt={handleInterruptPrimaryAction}
                      onImplementPlanInNewThread={handleImplementPlanInNewThreadPrimaryAction}
                    />
                  ) : null}
                </div>
              </div>
            </div>
          ) : null}

          {showCollapsedMobilePromptRow ? (
            <div className="flex items-center justify-between gap-2 px-3 py-2">
              <button
                type="button"
                className={cn(
                  "min-w-0 flex-1 truncate bg-transparent p-0 text-left text-[14px] focus:outline-none",
                  (activePendingProgress ? activePendingProgress.customAnswer : prompt.trim())
                    ? "text-foreground"
                    : "text-muted-foreground",
                )}
                onPointerDown={(event) => event.preventDefault()}
                onClick={expandMobileComposer}
                aria-label="Expand composer"
              >
                {activePendingProgress
                  ? activePendingProgress.customAnswer ||
                    "Type your own answer, or leave this blank to use the selected option"
                  : prompt.trim() || "Ask anything..."}
              </button>
              <button
                type="button"
                className="flex size-8 shrink-0 items-center justify-center rounded-full bg-primary/90 text-primary-foreground disabled:opacity-30"
                disabled={collapsedComposerPrimaryActionDisabled}
                aria-label={collapsedComposerPrimaryActionLabel}
                onPointerDown={(event) => event.preventDefault()}
                onClick={(event) => {
                  event.stopPropagation();
                  submitComposer();
                }}
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                  <path
                    d="M8 3L8 13M8 3L4 7M8 3L12 7"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  />
                </svg>
              </button>
            </div>
          ) : null}

          <div
            className={cn(
              "relative px-3 pb-2 sm:px-4",
              hasComposerHeader ? "pt-2.5 sm:pt-3" : "pt-3.5 sm:pt-4",
              isComposerCollapsedMobile && "hidden",
            )}
          >
            {composerMenuOpen && !isComposerApprovalState && (
              <div className="absolute inset-x-0 bottom-full z-20 mb-2">
                <ComposerCommandMenu
                  items={composerMenuItems}
                  resolvedTheme={resolvedTheme}
                  isLoading={isComposerMenuLoading}
                  loadingText={
                    isProviderCapabilityMenu && providerCapabilities.isPending
                      ? "Loading provider skills..."
                      : "Searching workspace files..."
                  }
                  issueText={capabilityIssue}
                  onRetry={providerCapabilities.refresh}
                  emptyStateText={composerMenuResult.emptyStateText}
                  activeItemId={activeComposerMenuItem?.id ?? null}
                  onHighlightedItemChange={onComposerMenuItemHighlighted}
                  onSelect={onSelectComposerItem}
                />
              </div>
            )}

            {!isComposerCollapsedMobile &&
              !isComposerApprovalState &&
              pendingUserInputs.length === 0 &&
              composerPreviewAnnotations.length > 0 && (
                <ComposerPreviewAnnotationCards
                  annotations={composerPreviewAnnotations}
                  images={composerImages}
                  onRemove={(annotationId) => {
                    if (isProviderBindingConflicted) return;
                    removeComposerDraftPreviewAnnotation(composerDraftTarget, annotationId);
                  }}
                  onExpandImage={(imageId) => {
                    const preview = buildExpandedImagePreview(composerImages, imageId);
                    if (preview) onExpandImage(preview);
                  }}
                  className="mb-3"
                />
              )}

            {!isComposerCollapsedMobile &&
              !isComposerApprovalState &&
              pendingUserInputs.length === 0 &&
              composerReviewComments.length > 0 && (
                <ComposerPendingReviewComments
                  comments={composerReviewComments}
                  onRemove={(commentId) => {
                    if (isProviderBindingConflicted) return;
                    removeComposerDraftReviewComment(composerDraftTarget, commentId);
                  }}
                  className="mb-3"
                />
              )}

            {!isComposerCollapsedMobile &&
              !isComposerApprovalState &&
              pendingUserInputs.length === 0 &&
              composerElementContexts.length > 0 && (
                <ComposerPendingElementContexts
                  contexts={composerElementContexts}
                  onRemove={(contextId) => {
                    if (isProviderBindingConflicted) return;
                    removeComposerDraftElementContext(composerDraftTarget, contextId);
                  }}
                  className="mb-3"
                />
              )}

            {!isComposerCollapsedMobile &&
              !isComposerApprovalState &&
              pendingUserInputs.length === 0 &&
              composerAttachments.some(
                (attachment) =>
                  attachment.type === "file" ||
                  !composerPreviewAnnotations.some((annotation) => annotation.id === attachment.id),
              ) && (
                <div className="mb-3 flex flex-wrap gap-2">
                  {composerAttachments
                    .filter(
                      (attachment) =>
                        attachment.type === "file" ||
                        !composerPreviewAnnotations.some(
                          (annotation) => annotation.id === attachment.id,
                        ),
                    )
                    .map((attachment) =>
                      attachment.type === "image" ? (
                        <div
                          key={attachment.id}
                          className="relative h-16 w-16 overflow-hidden rounded-lg border border-border/80 bg-background"
                        >
                          {attachment.previewUrl ? (
                            <button
                              type="button"
                              className="h-full w-full cursor-zoom-in"
                              aria-label={`Preview ${attachment.name}`}
                              onClick={() => {
                                const preview = buildExpandedImagePreview(
                                  composerImages,
                                  attachment.id,
                                );
                                if (!preview) return;
                                onExpandImage(preview);
                              }}
                            >
                              <img
                                src={attachment.previewUrl}
                                alt={attachment.name}
                                className="h-full w-full object-cover"
                              />
                            </button>
                          ) : (
                            <div className="flex h-full w-full items-center justify-center px-1 text-center text-xs text-muted-foreground">
                              {attachment.name}
                            </div>
                          )}
                          {nonPersistedComposerAttachmentIdSet.has(attachment.id) && (
                            <Tooltip>
                              <TooltipTrigger
                                render={
                                  <span
                                    role="img"
                                    aria-label="Draft attachment may not persist"
                                    className="absolute left-1 top-1 inline-flex items-center justify-center rounded bg-background/85 p-0.5 text-amber-600"
                                  >
                                    <CircleAlertIcon className="size-3" />
                                  </span>
                                }
                              />
                              <TooltipPopup
                                side="top"
                                className="max-w-64 whitespace-normal leading-tight"
                              >
                                Draft attachment could not be saved locally and may be lost on
                                navigation.
                              </TooltipPopup>
                            </Tooltip>
                          )}
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            className="absolute right-1 top-1 bg-background/80 hover:bg-background/90"
                            onClick={() => removeComposerAttachment(attachment.id)}
                            aria-label={`Remove ${attachment.name}`}
                            disabled={isProviderBindingConflicted}
                          >
                            <XIcon />
                          </Button>
                        </div>
                      ) : (
                        <div
                          key={attachment.id}
                          className="relative flex min-w-48 items-center gap-2 rounded-lg border border-border/80 bg-background px-2 py-1.5 pr-8"
                        >
                          <FileIcon className="size-4 shrink-0 text-muted-foreground" />
                          <div className="min-w-0">
                            <p className="truncate text-xs text-foreground">{attachment.name}</p>
                            <p className="text-xs text-muted-foreground">
                              {formatBytes(attachment.sizeBytes)}
                            </p>
                          </div>
                          {nonPersistedComposerAttachmentIdSet.has(attachment.id) && (
                            <Tooltip>
                              <TooltipTrigger
                                render={
                                  <span
                                    role="img"
                                    aria-label="Draft attachment may not persist"
                                    className="ml-auto inline-flex shrink-0 items-center justify-center text-amber-600"
                                  >
                                    <CircleAlertIcon className="size-3" />
                                  </span>
                                }
                              />
                              <TooltipPopup
                                side="top"
                                className="max-w-64 whitespace-normal leading-tight"
                              >
                                Draft attachment could not be saved locally and may be lost on
                                navigation.
                              </TooltipPopup>
                            </Tooltip>
                          )}
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            className="absolute right-1 top-1 bg-background/80 hover:bg-background/90"
                            onClick={() => removeComposerAttachment(attachment.id)}
                            aria-label={`Remove ${attachment.name}`}
                          >
                            <XIcon />
                          </Button>
                        </div>
                      ),
                    )}
                </div>
              )}

            <div className="relative">
              <ComposerPromptEditor
                editorRef={composerEditorRef}
                value={
                  isComposerApprovalState
                    ? ""
                    : activePendingProgress
                      ? activePendingProgress.customAnswer
                      : prompt
                }
                cursor={composerCursor}
                terminalContexts={
                  !isComposerApprovalState && pendingUserInputs.length === 0
                    ? composerTerminalContexts
                    : []
                }
                skills={providerCapabilities.data?.skills ?? []}
                agents={composerCapabilities.mentionableAgents}
                {...(showMobilePendingAnswerActions ? { className: "max-sm:pb-11" } : {})}
                onRemoveTerminalContext={removeComposerTerminalContextFromDraft}
                onChange={onPromptChange}
                onCommandKeyDown={onComposerCommandKey}
                onPaste={onComposerPaste}
                placeholder={
                  providerBindingConflictReason
                    ? providerBindingConflictReason
                    : isComposerApprovalState
                      ? (activePendingApproval?.detail ??
                        "Resolve this approval request to continue")
                      : activePendingProgress
                        ? "Type your own answer, or leave this blank to use the selected option"
                        : showPlanFollowUpPrompt && activeProposedPlan
                          ? "Add feedback to refine the plan, or leave this blank to implement it"
                          : environmentUnavailable
                            ? // The composer banner states the reason; this only names the state.
                              `${environmentUnavailable.label} is not connected`
                            : phase === "disconnected"
                              ? "Ask for follow-up changes or attach files"
                              : "Ask anything, @ files, : Coddy Studio actions, or a provider-native command"
                }
                disabled={
                  isConnecting ||
                  isProviderBindingConflicted ||
                  isComposerApprovalState ||
                  (environmentUnavailable !== null && activePendingProgress === null)
                }
              />
              {showMobilePendingAnswerActions ? (
                <div
                  data-chat-composer-mobile-pending-actions="true"
                  className="absolute bottom-0 right-0 flex justify-end"
                >
                  <ComposerPrimaryActions
                    compact
                    pendingAction={pendingPrimaryAction}
                    isRunning={false}
                    showPlanFollowUpPrompt={false}
                    promptHasText={false}
                    isSendBusy={isSendBusy}
                    isConnecting={isConnecting}
                    isEnvironmentUnavailable={environmentUnavailable !== null}
                    sendBlockedReason={providerBindingConflictReason}
                    isPreparingWorktree={false}
                    hasSendableContent={false}
                    preserveComposerFocusOnPointerDown
                    onPreviousPendingQuestion={onPreviousActivePendingUserInputQuestion}
                    onInterrupt={handleInterruptPrimaryAction}
                    onImplementPlanInNewThread={handleImplementPlanInNewThreadPrimaryAction}
                  />
                </div>
              ) : null}
            </div>
          </div>

          {/* Bottom toolbar */}
          {isComposerCollapsedMobile ? null : activePendingApproval ? (
            <div className="flex items-center justify-end gap-2 px-2.5 pb-2.5 sm:px-3 sm:pb-3">
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      type="button"
                      aria-label="Attach files"
                      disabled
                    />
                  }
                >
                  <PaperclipIcon />
                </TooltipTrigger>
                <TooltipPopup side="top">Attach files</TooltipPopup>
              </Tooltip>
              <ComposerPendingApprovalActions
                requestId={activePendingApproval.requestId}
                isResponding={respondingRequestIds.includes(activePendingApproval.requestId)}
                onRespondToApproval={onRespondToApproval}
              />
            </div>
          ) : (
            <div
              data-chat-composer-footer="true"
              data-chat-composer-footer-compact={isComposerFooterCompact ? "true" : "false"}
              className={cn(
                "flex min-w-0 flex-nowrap items-center justify-between gap-2 overflow-visible px-2.5 pb-2.5 sm:px-3 sm:pb-3",
                pendingUserInputs.length > 0 && "pt-2",
                isComposerFooterCompact ? "gap-1.5" : "gap-2 sm:gap-0",
                showMobilePendingAnswerActions && "hidden sm:flex",
              )}
            >
              <div className="-m-1 flex min-w-0 flex-1 items-center gap-1 overflow-x-auto p-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
                <ProviderModelPicker
                  compact={isComposerFooterCompact}
                  activeInstanceId={selectedInstanceId}
                  model={selectedModelForPickerWithCustomFallback}
                  lockToActiveInstance={lockProviderPickerToActiveInstance}
                  disabled={isProviderBindingConflicted}
                  lockedProvider={lockedProvider}
                  lockedContinuationGroupKey={lockedContinuationGroupKey}
                  instanceEntries={providerInstanceEntries}
                  keybindings={keybindings}
                  modelOptionsByInstance={modelOptionsByInstance}
                  terminalOpen={terminalOpen}
                  open={isComposerModelPickerOpen}
                  {...(composerProviderState.modelPickerIconClassName
                    ? {
                        activeProviderIconClassName: composerProviderState.modelPickerIconClassName,
                      }
                    : {})}
                  onOpenChange={(open) => {
                    if (isProviderBindingConflicted) return;
                    setIsComposerModelPickerOpen(open);
                  }}
                  getModelDisabledReason={getModelDisabledReason}
                  onInstanceModelChange={(instanceId, model) => {
                    if (isProviderBindingConflicted) return;
                    onProviderModelSelect(instanceId, model);
                  }}
                />

                {composerTraitControls}
                <ComposerFooterModeControls
                  interactionModeAvailability={composerProviderControls.interactionModeAvailability}
                  interactionMode={interactionMode}
                  runtimeMode={runtimeMode}
                  showPlanToggle={showPlanSidebarToggle}
                  planSidebarLabel={planSidebarLabel}
                  planSidebarOpen={planSidebarOpen}
                  onToggleInteractionMode={toggleInteractionMode}
                  onRuntimeModeChange={handleRuntimeModeChange}
                  onTogglePlanSidebar={togglePlanSidebar}
                />
              </div>

              {/* Right side: send / stop button */}
              <div
                data-chat-composer-actions="right"
                data-chat-composer-primary-actions-compact={
                  isComposerPrimaryActionsCompact ? "true" : "false"
                }
                className="flex shrink-0 flex-nowrap items-center justify-end gap-2"
              >
                <ComposerFooterPrimaryActions
                  compact={isComposerPrimaryActionsCompact}
                  activeContextWindow={activeContextWindow}
                  activeMcpStatus={activeMcpStatus}
                  activeThreadProviderDisplayName={activeThreadProviderDisplayName}
                  supportsMcpStatus={supportsMcpStatus}
                  supportsContextWindowUsage={supportsContextWindowUsage}
                  showContextWindowMeter={pendingUserInputs.length === 0}
                  pendingAction={pendingPrimaryAction}
                  isRunning={phase === "running"}
                  canCancelPendingSend={canCancelPendingSend}
                  showPlanFollowUpPrompt={pendingUserInputs.length === 0 && showPlanFollowUpPrompt}
                  promptHasText={prompt.trim().length > 0}
                  isSendBusy={isSendBusy}
                  isConnecting={isConnecting}
                  isEnvironmentUnavailable={environmentUnavailable !== null}
                  sendBlockedReason={providerBindingConflictReason}
                  isPreparingWorktree={isPreparingWorktree}
                  hasSendableContent={composerSendState.hasSendableContent}
                  isAttachmentSelectionDisabled={
                    isProviderBindingConflicted ||
                    isComposerApprovalState ||
                    pendingUserInputs.length > 0
                  }
                  preserveComposerFocusOnPointerDown={isMobileViewport}
                  onSelectAttachments={() => composerFileInputRef.current?.click()}
                  onPreviousPendingQuestion={onPreviousActivePendingUserInputQuestion}
                  onInterrupt={handleInterruptPrimaryAction}
                  onImplementPlanInNewThread={handleImplementPlanInNewThreadPrimaryAction}
                />
              </div>
            </div>
          )}
        </div>
      </div>
    </form>
  );
});
