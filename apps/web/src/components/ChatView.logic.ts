import type { StagedAttachmentAttempt } from "../state/attachmentAdmissions";
import {
  ACTIVITY_PAGE_MAX_LENGTH,
  type ActivityScopeRef,
  type EnvironmentId,
  type MessageId,
  isProviderDriverKind,
  PROVIDER_DISPLAY_NAMES,
  ProjectId,
  type ModelSelection,
  type OrchestrationSessionStatus,
  type TurnDelivery,
  type ProviderDriverKind,
  type ProviderInstanceId,
  type ServerProvider,
  type ScopedThreadRef,
  type ThreadId,
  type TurnId,
} from "@bibcode/contracts";
import { type ChatMessage, type SessionPhase, type Thread } from "../types";
import { formatProviderDriverKindLabel } from "../providerModels";
import { providerDriverLabel } from "../providerInstances";
import { type ComposerAttachment, type DraftThreadState } from "../composerDraftStore";
import * as Schema from "effect/Schema";
import { appAtomRegistry } from "../rpc/atomRegistry";
import { environmentThreadDetails } from "../state/threads";
import {
  filterTerminalContextsWithText,
  stripInlineTerminalContextPlaceholders,
  type TerminalContextDraft,
} from "../lib/terminalContext";
import type { DraftThreadEnvMode } from "../composerDraftStore";
import {
  connectionStatusText,
  type EnvironmentConnectionPresentation,
} from "@bibcode/client-runtime/connection";
import type { AttachmentSource, AttachmentStagingResult } from "@bibcode/client-runtime/operations";
import { settlePromise, type AtomCommandResult } from "@bibcode/client-runtime/state/runtime";
import { AsyncResult } from "effect/unstable/reactivity";
import type { StagedUploadChatAttachment, UploadChatAttachment } from "@bibcode/contracts";

export const LAST_INVOKED_SCRIPT_BY_PROJECT_KEY = "bibcode:last-invoked-script-by-project";
export const MAX_HIDDEN_MOUNTED_PREVIEW_THREADS = 3;

export const LastInvokedScriptByProjectSchema = Schema.Record(ProjectId, Schema.String);

type ActivitySurfaceScope =
  | { readonly _tag: "thread" }
  | { readonly _tag: "terminal"; readonly terminalId: string };

export function isAgentActivityScopeEnabled(
  scope: ActivityScopeRef,
  settings: {
    readonly enableChatAgentActivity: boolean;
    readonly enableTerminalAgentActivity: boolean;
  },
): boolean {
  return scope._tag === "terminal"
    ? settings.enableTerminalAgentActivity
    : settings.enableChatAgentActivity;
}

export function resolveActivityScope(
  activeThreadRef: ScopedThreadRef | null,
  surfaceScope: ActivitySurfaceScope,
): ActivityScopeRef | null {
  if (activeThreadRef === null) {
    return null;
  }
  return surfaceScope._tag === "terminal"
    ? {
        _tag: "terminal",
        threadId: activeThreadRef.threadId,
        terminalId: surfaceScope.terminalId,
      }
    : { _tag: "thread", threadId: activeThreadRef.threadId };
}

type ActivityPageLike =
  | {
      readonly records: ReadonlyArray<unknown>;
      readonly nextCursor: string | null;
    }
  | {
      readonly entries: ReadonlyArray<unknown>;
      readonly nextCursor: string | null;
    };

export interface KeyedActivityPage<Page> {
  readonly cursor: string | null;
  readonly page: Page;
}

function activityPageItemCount(page: ActivityPageLike): number {
  return "records" in page ? page.records.length : page.entries.length;
}

function trimActivityPageStart<Page extends ActivityPageLike>(page: Page, count: number): Page {
  if ("records" in page) {
    return {
      ...page,
      records: page.records.slice(count),
    } as Page;
  }
  return {
    ...page,
    entries: page.entries.slice(count),
  } as Page;
}

export function reconcileBoundedActivityPages<Page extends ActivityPageLike>(
  current: ReadonlyArray<KeyedActivityPage<Page>>,
  cursor: string | null,
  incoming: Page,
): ReadonlyArray<KeyedActivityPage<Page>> {
  const existingIndex = current.findIndex((entry) => entry.cursor === cursor);
  const candidates =
    existingIndex < 0
      ? [...current, { cursor, page: incoming }]
      : current.map((entry, index) =>
          index === existingIndex ? { cursor, page: incoming } : entry,
        );
  const totalItems = candidates.reduce(
    (count, candidate) => count + activityPageItemCount(candidate.page),
    0,
  );
  let overflow = Math.max(0, totalItems - ACTIVITY_PAGE_MAX_LENGTH);
  const bounded: Array<KeyedActivityPage<Page>> = [];

  for (const candidate of candidates) {
    const itemCount = activityPageItemCount(candidate.page);
    if (overflow >= itemCount) {
      overflow -= itemCount;
      continue;
    }
    if (overflow > 0) {
      bounded.push({
        cursor: candidate.cursor,
        page: trimActivityPageStart(candidate.page, overflow),
      });
      overflow = 0;
      continue;
    }
    bounded.push(candidate);
  }
  return bounded;
}

export function resolveActivityPageLoadAction(input: {
  readonly currentCursor: string | null;
  readonly nextCursor: string | null;
  readonly failed: boolean;
}): "refresh" | "select" {
  return input.nextCursor === null || (input.failed && input.currentCursor === input.nextCursor)
    ? "refresh"
    : "select";
}

export function buildLocalDraftThread(
  threadId: ThreadId,
  draftThread: DraftThreadState,
  fallbackModelSelection: ModelSelection,
): Thread {
  return {
    id: threadId,
    environmentId: draftThread.environmentId,
    projectId: draftThread.projectId,
    title: "New thread",
    modelSelection: fallbackModelSelection,
    runtimeMode: draftThread.runtimeMode,
    interactionMode: draftThread.interactionMode,
    session: null,
    messages: [],
    createdAt: draftThread.createdAt,
    updatedAt: draftThread.createdAt,
    archivedAt: null,
    deletedAt: null,
    latestTurn: null,
    branch: draftThread.branch,
    worktreePath: draftThread.worktreePath,
    checkpoints: [],
    activities: [],
    proposedPlans: [],
  };
}

export function resolveCenterPanelLaunchContext(input: {
  readonly hasServerThread: boolean;
  readonly envMode: DraftThreadEnvMode;
  readonly projectCwd: string | null;
  readonly worktreePath: string | null;
}): { readonly cwd: string; readonly worktreePath: string | null } | null {
  if (!input.hasServerThread || input.projectCwd === null) {
    return null;
  }
  if (input.envMode === "worktree") {
    return input.worktreePath === null
      ? null
      : { cwd: input.worktreePath, worktreePath: input.worktreePath };
  }
  return { cwd: input.projectCwd, worktreePath: null };
}

export function shouldWriteThreadErrorToCurrentServerThread(input: {
  serverThread:
    | {
        environmentId: EnvironmentId;
        id: ThreadId;
      }
    | null
    | undefined;
  routeThreadRef: ScopedThreadRef;
  targetThreadId: ThreadId;
}): boolean {
  return Boolean(
    input.serverThread &&
    input.targetThreadId === input.routeThreadRef.threadId &&
    input.serverThread.environmentId === input.routeThreadRef.environmentId &&
    input.serverThread.id === input.targetThreadId,
  );
}

export function buildThreadTurnInterruptInput(thread: Pick<Thread, "id" | "session">): {
  threadId: ThreadId;
  turnId?: TurnId;
} {
  const runningTurnId = thread.session?.status === "running" ? thread.session.activeTurnId : null;
  return {
    threadId: thread.id,
    ...(runningTurnId !== null ? { turnId: runningTurnId } : {}),
  };
}

export function reconcileRetainedMountedThreadIds(input: {
  currentThreadIds: ReadonlyArray<string>;
  openThreadIds: ReadonlyArray<string>;
  activeThreadId: string | null;
  activeThreadOpen: boolean;
  maxHiddenThreadCount: number;
  retainInactiveActiveThread?: boolean;
}): string[] {
  const openThreadIdSet = new Set(input.openThreadIds);
  const hiddenThreadIds = input.currentThreadIds.filter(
    (threadId) =>
      (threadId !== input.activeThreadId || input.retainInactiveActiveThread === true) &&
      openThreadIdSet.has(threadId),
  );
  const maxHiddenThreadCount = Math.max(0, input.maxHiddenThreadCount);
  const nextThreadIds =
    hiddenThreadIds.length > maxHiddenThreadCount
      ? hiddenThreadIds.slice(-maxHiddenThreadCount)
      : hiddenThreadIds;

  if (
    input.activeThreadId &&
    input.activeThreadOpen &&
    !nextThreadIds.includes(input.activeThreadId)
  ) {
    nextThreadIds.push(input.activeThreadId);
  }

  return nextThreadIds;
}

export function revokeBlobPreviewUrl(previewUrl: string | undefined): void {
  if (!previewUrl || typeof URL === "undefined" || !previewUrl.startsWith("blob:")) {
    return;
  }
  URL.revokeObjectURL(previewUrl);
}

export function revokeUserMessagePreviewUrls(message: ChatMessage): void {
  if (message.role !== "user" || !message.attachments) {
    return;
  }
  for (const attachment of message.attachments) {
    if (attachment.type !== "image") {
      continue;
    }
    revokeBlobPreviewUrl(attachment.previewUrl);
  }
}

export function collectUserMessageBlobPreviewUrls(message: ChatMessage): string[] {
  if (message.role !== "user" || !message.attachments) {
    return [];
  }
  const previewUrls: string[] = [];
  for (const attachment of message.attachments) {
    if (attachment.type !== "image") continue;
    if (!attachment.previewUrl || !attachment.previewUrl.startsWith("blob:")) continue;
    previewUrls.push(attachment.previewUrl);
  }
  return previewUrls;
}

export interface PullRequestDialogState {
  initialReference: string | null;
  key: number;
}

export function readFileAsDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
        return;
      }
      reject(new Error("Could not read attachment data."));
    });
    reader.addEventListener("error", () => {
      reject(reader.error ?? new Error("Failed to read attachment."));
    });
    reader.readAsDataURL(file);
  });
}

export function resolveSendEnvMode(input: {
  requestedEnvMode: DraftThreadEnvMode;
  isGitRepo: boolean;
}): DraftThreadEnvMode {
  return input.isGitRepo ? input.requestedEnvMode : "local";
}

export function cloneComposerAttachmentForRetry(
  attachment: ComposerAttachment,
): ComposerAttachment {
  if (
    attachment.type !== "image" ||
    typeof URL === "undefined" ||
    !attachment.previewUrl.startsWith("blob:")
  ) {
    return attachment;
  }
  try {
    return {
      ...attachment,
      previewUrl: URL.createObjectURL(attachment.file),
    };
  } catch {
    return attachment;
  }
}

export function deriveComposerSendState(options: {
  prompt: string;
  imageCount: number;
  terminalContexts: ReadonlyArray<TerminalContextDraft>;
  /**
   * Optional element-pick attachment count. Element contexts contribute to
   * "sendable content" exactly like images and (text-bearing) terminal
   * contexts do: a prompt of just element chips is still a valid send.
   */
  elementContextCount?: number;
}): {
  trimmedPrompt: string;
  sendableTerminalContexts: TerminalContextDraft[];
  expiredTerminalContextCount: number;
  hasSendableContent: boolean;
} {
  const trimmedPrompt = stripInlineTerminalContextPlaceholders(options.prompt).trim();
  const sendableTerminalContexts = filterTerminalContextsWithText(options.terminalContexts);
  const expiredTerminalContextCount =
    options.terminalContexts.length - sendableTerminalContexts.length;
  const elementContextCount = options.elementContextCount ?? 0;
  return {
    trimmedPrompt,
    sendableTerminalContexts,
    expiredTerminalContextCount,
    hasSendableContent:
      trimmedPrompt.length > 0 ||
      options.imageCount > 0 ||
      sendableTerminalContexts.length > 0 ||
      elementContextCount > 0,
  };
}

export function buildExpiredTerminalContextToastCopy(
  expiredTerminalContextCount: number,
  variant: "omitted" | "empty",
): { title: string; description: string } {
  const count = Math.max(1, Math.floor(expiredTerminalContextCount));
  const noun = count === 1 ? "Expired terminal context" : "Expired terminal contexts";
  if (variant === "empty") {
    return {
      title: `${noun} won't be sent`,
      description: "Remove it or re-add it to include terminal output.",
    };
  }
  return {
    title: `${noun} omitted from message`,
    description: "Re-add it if you want that terminal output included.",
  };
}

export function threadHasStarted(thread: Thread | null | undefined): boolean {
  return Boolean(
    thread && (thread.latestTurn !== null || thread.messages.length > 0 || thread.session !== null),
  );
}

// Both provider inputs are open branded driver slugs. Callers should resolve a
// bound instance id to its live driver before passing `threadProvider`; an
// unresolved exact-instance binding must stay separate rather than being
// rebranded as a driver kind because the two open slug namespaces can collide.
export function deriveLockedProvider(input: {
  thread: Thread | null | undefined;
  selectedProvider: string | null;
  threadProvider: string | null;
}): ProviderDriverKind | null {
  if (!threadHasStarted(input.thread)) {
    return null;
  }
  const sessionProvider = input.thread?.session?.providerName ?? null;
  if (sessionProvider && isProviderDriverKind(sessionProvider)) {
    return sessionProvider;
  }
  const narrowedThreadProvider =
    input.threadProvider && isProviderDriverKind(input.threadProvider)
      ? input.threadProvider
      : null;
  const narrowedSelectedProvider =
    input.selectedProvider && isProviderDriverKind(input.selectedProvider)
      ? input.selectedProvider
      : null;
  return narrowedThreadProvider ?? narrowedSelectedProvider ?? null;
}

export function getStartedThreadModelChangeBlockReason(input: {
  providers: ReadonlyArray<Pick<ServerProvider, "instanceId" | "requiresNewThreadForModelChange">>;
  hasStartedSession: boolean;
  currentModelSelection: ModelSelection;
  currentProviderInstanceId?: ModelSelection["instanceId"] | null | undefined;
  nextModelSelection: ModelSelection;
}): { title: string; description: string } | null {
  if (!input.hasStartedSession) {
    return null;
  }
  const currentModelSelection = {
    ...input.currentModelSelection,
    instanceId: input.currentProviderInstanceId ?? input.currentModelSelection.instanceId,
  };
  if (
    currentModelSelection.instanceId === input.nextModelSelection.instanceId &&
    currentModelSelection.model === input.nextModelSelection.model
  ) {
    return null;
  }
  const currentProvider = input.providers.find(
    (snapshot) => snapshot.instanceId === currentModelSelection.instanceId,
  );
  const nextProvider = input.providers.find(
    (snapshot) => snapshot.instanceId === input.nextModelSelection.instanceId,
  );
  if (
    currentProvider?.requiresNewThreadForModelChange !== true &&
    nextProvider?.requiresNewThreadForModelChange !== true
  ) {
    return null;
  }
  return {
    title: "Start a new chat to change models",
    description: "This provider does not allow switching models after a conversation has started.",
  };
}

export async function waitForStartedServerThread(
  threadRef: ScopedThreadRef,
  timeoutMs = 1_000,
): Promise<boolean> {
  const threadAtom = environmentThreadDetails.detailAtom(threadRef);
  const getThread = () => appAtomRegistry.get(threadAtom);
  const thread = getThread();

  if (threadHasStarted(thread)) {
    return true;
  }

  return await new Promise<boolean>((resolve) => {
    let settled = false;
    let timeoutId: ReturnType<typeof globalThis.setTimeout> | null = null;
    const finish = (result: boolean) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timeoutId !== null) {
        globalThis.clearTimeout(timeoutId);
      }
      unsubscribe();
      resolve(result);
    };

    const unsubscribe = appAtomRegistry.subscribe(threadAtom, (thread) => {
      if (!threadHasStarted(thread)) {
        return;
      }
      finish(true);
    });

    if (threadHasStarted(getThread())) {
      finish(true);
      return;
    }

    timeoutId = globalThis.setTimeout(() => {
      finish(false);
    }, timeoutMs);
  });
}

export interface LocalDispatchSnapshot {
  startedAt: string;
  preparingWorktree: boolean;
  threadId: ThreadId | null;
  messageId: MessageId | null;
  latestTurnTurnId: TurnId | null;
  latestTurnRequestedAt: string | null;
  latestTurnStartedAt: string | null;
  latestTurnCompletedAt: string | null;
  sessionStatus: NonNullable<Thread["session"]>["status"] | null;
  sessionUpdatedAt: string | null;
  threadError: string | null;
}

export function findLastCancellableDeliveryMessage(
  messages: ReadonlyArray<ChatMessage>,
): ChatMessage | null {
  return (
    messages.findLast(
      (message) =>
        !isQueuedTimelineMessage(message) &&
        (message.delivery?.state === "pending" || message.delivery?.state === "sending"),
    ) ?? null
  );
}

export function findActiveDeliveryMessage(
  messages: ReadonlyArray<ChatMessage>,
): ChatMessage | null {
  const oldestUnresolved = messages.find((message) => {
    if (isQueuedTimelineMessage(message)) return false;
    const state = message.delivery?.state;
    return (
      state === "pending" || state === "sending" || state === "uncertain" || state === "failed"
    );
  });
  const state = oldestUnresolved?.delivery?.state;
  return state === "pending" || state === "sending" ? (oldestUnresolved ?? null) : null;
}

export function deliveryOffersRetry(delivery: TurnDelivery): boolean {
  return delivery.state !== "failed" || delivery.reason !== "modelSelectionRefused";
}

export function findBlockingDelivery(messages: ReadonlyArray<ChatMessage>): ChatMessage | null {
  return (
    messages.find(
      (message) =>
        !isQueuedTimelineMessage(message) &&
        (message.delivery?.state === "failed" || message.delivery?.state === "uncertain"),
    ) ?? null
  );
}

/** W1 order proxy (2026-09-26-turn-delivery-failure-reason-design.md): only later timestamps prove a wait. */
export function waitsBehind(
  message: Pick<ChatMessage, "id" | "createdAt">,
  blocker: Pick<ChatMessage, "id" | "createdAt"> | null,
): boolean {
  return (
    blocker !== null &&
    message.id !== blocker.id &&
    Date.parse(message.createdAt) > Date.parse(blocker.createdAt)
  );
}

export function deliveryProviderLabel(
  delivery: Pick<TurnDelivery, "provider" | "providerInstanceId">,
  instanceLabels: ReadonlyMap<ProviderInstanceId, string>,
): string {
  return (
    (delivery.providerInstanceId !== undefined
      ? instanceLabels.get(delivery.providerInstanceId)
      : undefined) ?? providerDriverLabel(delivery.provider)
  );
}

/**
 * The confirmation for "Revert to this message". A provider that cannot rewind its conversation
 * (`supportsConversationRewind: false`) gets only its files back and keeps the newer messages.
 * Anything else, including an older server that omits the flag, keeps the full-revert warning.
 */
export function buildRevertCheckpointConfirmation(input: {
  turnCount: number;
  provider: Pick<ServerProvider, "displayName" | "driver" | "supportsConversationRewind"> | null;
}): string {
  if (input.provider?.supportsConversationRewind === false) {
    const name = input.provider.displayName?.trim() || providerDriverLabel(input.provider.driver);
    return [
      `Restore files to checkpoint ${input.turnCount}?`,
      `${name} can't rewind its conversation, so the newer messages stay.`,
      "This action cannot be undone.",
    ].join("\n");
  }
  return [
    `Revert this thread to checkpoint ${input.turnCount}?`,
    "This will discard newer messages and turn diffs in this thread.",
    "This action cannot be undone.",
  ].join("\n");
}

export function isQueuedTimelineMessage(message: ChatMessage): boolean {
  const delivery = message.delivery;
  return (
    delivery?.state === "queued" ||
    (delivery?.mode === "steer" && (delivery.state === "pending" || delivery.state === "sending"))
  );
}

export function selectQueuedMessages(messages: ReadonlyArray<ChatMessage>): ChatMessage[] {
  // The snapshot preserves durable FIFO order, even when timestamps disagree.
  return messages.filter(isQueuedTimelineMessage);
}

export function shouldEnqueueOnSend(input: {
  phase: SessionPhase;
  sessionStatus: OrchestrationSessionStatus | null;
  hasPendingDelivery: boolean;
}): boolean {
  return (
    input.phase === "running" || input.sessionStatus === "starting" || input.hasPendingDelivery
  );
}

export interface QueuedCardStatus {
  label: string;
  canSteer: boolean;
  steerDisabledReason: string | null;
  canCancel: boolean;
  steering: boolean;
  primaryAction: "steer" | "send-now" | null;
  canSendNow: boolean;
  sendNowDisabledReason: string | null;
}

export function deriveQueuedCardStatus(input: {
  index: number;
  phase: SessionPhase;
  sessionStatus: OrchestrationSessionStatus | null;
  supportsTurnSteer: boolean;
  delivery: TurnDelivery;
  hasPendingApproval: boolean;
  hasPendingUserInput: boolean;
  waitingBehind: { readonly offersRetry: boolean } | null;
}): QueuedCardStatus {
  const steering =
    input.delivery.mode === "steer" &&
    (input.delivery.state === "pending" || input.delivery.state === "sending");
  const running = input.phase === "running";
  const waiting = input.delivery.held === true || !running;
  const blockedReason = steering
    ? "Steering…"
    : input.index > 0
      ? "Send the messages above first"
      : input.waitingBehind !== null
        ? input.waitingBehind.offersRetry
          ? "Retry or dismiss the earlier message first"
          : "Dismiss the earlier message first"
        : input.hasPendingApproval
          ? "Respond to the pending approval first"
          : input.hasPendingUserInput
            ? "Answer the pending question first"
            : null;
  const steerDisabledReason =
    blockedReason ??
    (!input.supportsTurnSteer
      ? "This provider cannot steer a running turn"
      : waiting
        ? "Waiting for you"
        : null);
  const sendNowDisabledReason =
    blockedReason ??
    (running
      ? "Wait for the running turn to end"
      : input.sessionStatus === "starting"
        ? "Wait for the session to finish starting"
        : null);
  return {
    label: steering
      ? "Steering…"
      : input.index > 0
        ? "Sends after the messages above."
        : input.waitingBehind !== null
          ? "Waiting for an earlier message"
          : waiting
            ? "Waiting for you"
            : input.supportsTurnSteer
              ? "Sends when the turn ends. Steer to send it now."
              : "Sends when the turn ends.",
    canSteer: steerDisabledReason === null,
    steerDisabledReason,
    canCancel: input.delivery.state === "queued",
    steering,
    primaryAction:
      input.index > 0 ? null : waiting ? "send-now" : input.supportsTurnSteer ? "steer" : null,
    canSendNow: sendNowDisabledReason === null,
    sendNowDisabledReason,
  };
}

export function createLocalDispatchSnapshot(
  activeThread: Thread | undefined,
  options?: {
    preparingWorktree?: boolean;
    threadId?: ThreadId;
    messageId?: MessageId;
    threadError?: string | null;
  },
): LocalDispatchSnapshot {
  const latestTurn = activeThread?.latestTurn ?? null;
  const session = activeThread?.session ?? null;
  return {
    startedAt: new Date().toISOString(),
    preparingWorktree: Boolean(options?.preparingWorktree),
    threadId: options?.threadId ?? activeThread?.id ?? null,
    messageId: options?.messageId ?? null,
    latestTurnTurnId: latestTurn?.turnId ?? null,
    latestTurnRequestedAt: latestTurn?.requestedAt ?? null,
    latestTurnStartedAt: latestTurn?.startedAt ?? null,
    latestTurnCompletedAt: latestTurn?.completedAt ?? null,
    sessionStatus: session?.status ?? null,
    sessionUpdatedAt: session?.updatedAt ?? null,
    threadError: options?.threadError ?? null,
  };
}

export function hasServerAcknowledgedLocalDispatch(input: {
  localDispatch: LocalDispatchSnapshot | null;
  phase: SessionPhase;
  latestTurn: Thread["latestTurn"] | null;
  session: Thread["session"] | null;
  hasPendingApproval: boolean;
  hasPendingUserInput: boolean;
  threadError: string | null | undefined;
  deliveryState?: NonNullable<ChatMessage["delivery"]>["state"] | null;
}): boolean {
  if (!input.localDispatch) {
    return false;
  }
  const currentThreadError = input.threadError ?? null;
  if (
    input.hasPendingApproval ||
    input.hasPendingUserInput ||
    (currentThreadError !== null && currentThreadError !== input.localDispatch.threadError)
  ) {
    return true;
  }
  if (input.deliveryState !== null && input.deliveryState !== undefined) {
    return true;
  }

  const latestTurn = input.latestTurn ?? null;
  const session = input.session ?? null;
  const latestTurnChanged =
    input.localDispatch.latestTurnTurnId !== (latestTurn?.turnId ?? null) ||
    input.localDispatch.latestTurnRequestedAt !== (latestTurn?.requestedAt ?? null) ||
    input.localDispatch.latestTurnStartedAt !== (latestTurn?.startedAt ?? null) ||
    input.localDispatch.latestTurnCompletedAt !== (latestTurn?.completedAt ?? null);

  if (input.phase === "running") {
    if (!latestTurnChanged) {
      return false;
    }
    if (latestTurn?.startedAt === null || latestTurn === null) {
      return false;
    }
    if (
      session?.activeTurnId !== null &&
      session?.activeTurnId !== undefined &&
      latestTurn?.turnId !== session.activeTurnId
    ) {
      return false;
    }
    return true;
  }

  return (
    latestTurnChanged ||
    input.localDispatch.sessionStatus !== (session?.status ?? null) ||
    input.localDispatch.sessionUpdatedAt !== (session?.updatedAt ?? null)
  );
}

/**
 * Who reported the error shown in the thread error banner.
 *
 * `session.lastError` is mixed-provenance: it carries both a provider's own
 * failure and BiBCode's restart notice, and `localServerError` carries a
 * BiBCode command that could not be completed. Rendering all of them
 * identically made an upstream provider outage read as a BiBCode defect.
 *
 * This states who *reported* the failure, never who is to blame — a provider
 * rejecting a malformed BiBCode request is still provider-reported, and a
 * failed BiBCode command may have failed because the provider refused it.
 * Returns `null` when nothing truthful can be said, in which case the banner
 * renders exactly as before.
 */
export function threadErrorAttribution(input: {
  readonly isBiBCodeAction: boolean;
  readonly errorClass: string | null | undefined;
  readonly providerName: string | null | undefined;
}): string | null {
  if (input.isBiBCodeAction) {
    return "BiBCode could not complete this action";
  }
  const provider =
    input.providerName && isProviderDriverKind(input.providerName)
      ? (PROVIDER_DISPLAY_NAMES[input.providerName] ??
        formatProviderDriverKindLabel(input.providerName))
      : null;
  const agent = provider ?? "The agent";
  switch (input.errorClass) {
    case "provider_error":
      return `${agent} reported an error`;
    case "transport_error":
      return `BiBCode lost its connection to ${provider ?? "the agent"}`;
    case "session_stopped":
      return "BiBCode stopped this session";
    case "permission_error":
      return `${agent} rejected the credentials`;
    case "validation_error":
      return `${agent} rejected the request`;
    case "context_limit":
      return `${agent} ran out of context`;
    default:
      // "unknown", absent, or a class from a newer server: say nothing rather
      // than guess. An unattributed banner is better than a wrong attribution.
      return null;
  }
}

/** The message the thread error's Compact action sends: the provider's own compaction command. */
export const COMPACT_CONVERSATION_COMMAND = "/compact";

/**
 * Whether the thread error offers to compact the conversation, and why it cannot yet.
 *
 * Only a provider's `context_limit` failure offers it: the conversation no longer fits the
 * context window, and compacting it is how the user continues. While the thread is working (a
 * running turn, which includes a compaction, or a message still being sent) the action is
 * disabled with its reason, so it can never queue a second compaction.
 */
export function threadErrorCompactAction(input: {
  readonly isBiBCodeAction: boolean;
  readonly errorClass: string | null | undefined;
  readonly busy: boolean;
}): { readonly disabledReason: string | null } | null {
  if (input.isBiBCodeAction || input.errorClass !== "context_limit") {
    return null;
  }
  return {
    disabledReason: input.busy ? "Wait for the current work to finish, then compact." : null,
  };
}

/**
 * The composer banner for an environment that is not connected: a short
 * state in the title, and the connection's reason, once, in the body.
 */
export function describeUnavailableEnvironment(input: {
  readonly label: string;
  readonly connection: EnvironmentConnectionPresentation;
}): { readonly title: string; readonly description: string } {
  return {
    title: `${input.label}: ${connectionStatusText({ phase: input.connection.phase, error: null, traceId: null })}`,
    description:
      input.connection.notice ??
      input.connection.error ??
      "Reconnect this environment before sending messages or running actions.",
  };
}

/** Staging selects the wire path before any whole-file encoding starts. */
export async function prepareTurnAttachments(input: {
  readonly sources: ReadonlyArray<AttachmentSource>;
  readonly stage: () => Promise<AtomCommandResult<AttachmentStagingResult, unknown>>;
  readonly readInline?: (file: Blob) => Promise<string>;
}): Promise<AtomCommandResult<ReadonlyArray<UploadChatAttachment>, unknown>> {
  const result = await input.stage();
  if (result._tag === "Failure") return AsyncResult.failure(result.cause);
  if (result.value._tag === "staged") return AsyncResult.success(result.value.attachments);
  const read = input.readInline ?? readFileAsDataUrl;
  return settlePromise(() =>
    Promise.all(
      input.sources.map(async (source) => ({
        type: source.type,
        id: source.id,
        name: source.name,
        mimeType: source.mimeType,
        sizeBytes: source.sizeBytes,
        dataUrl: await read(source.file),
      })),
    ),
  );
}

interface ComposerContentLike {
  readonly prompt: string;
  readonly attachments: ReadonlyArray<{ readonly id: string }>;
  readonly terminalContexts: ReadonlyArray<{ readonly id: string }>;
  readonly elementContexts: ReadonlyArray<{ readonly id: string }>;
  readonly previewAnnotations: ReadonlyArray<{ readonly id: string }>;
  readonly reviewComments: ReadonlyArray<{ readonly id: string }>;
}

function mergeContentById<
  Current extends { readonly id: string },
  Outgoing extends { readonly id: string },
>(current: ReadonlyArray<Current>, outgoing: ReadonlyArray<Outgoing>): Array<Current | Outgoing> {
  const ids = new Set(current.map((item) => item.id));
  return [...current, ...outgoing.filter((item) => !ids.has(item.id))];
}

/** A failed or cancelled send must restore the snapshot without replacing newer work. */
export function mergeInterruptedComposerContent<
  Current extends ComposerContentLike,
  Outgoing extends ComposerContentLike,
>(current: Current, outgoing: Outgoing) {
  return {
    prompt:
      current.prompt === outgoing.prompt
        ? current.prompt
        : [current.prompt, outgoing.prompt].filter(Boolean).join("\n\n"),
    attachments: mergeContentById<Current["attachments"][number], Outgoing["attachments"][number]>(
      current.attachments,
      outgoing.attachments,
    ),
    terminalContexts: mergeContentById<
      Current["terminalContexts"][number],
      Outgoing["terminalContexts"][number]
    >(current.terminalContexts, outgoing.terminalContexts),
    elementContexts: mergeContentById<
      Current["elementContexts"][number],
      Outgoing["elementContexts"][number]
    >(current.elementContexts, outgoing.elementContexts),
    previewAnnotations: mergeContentById<
      Current["previewAnnotations"][number],
      Outgoing["previewAnnotations"][number]
    >(current.previewAnnotations, outgoing.previewAnnotations),
    reviewComments: mergeContentById<
      Current["reviewComments"][number],
      Outgoing["reviewComments"][number]
    >(current.reviewComments, outgoing.reviewComments),
  };
}

/** Upload ids belong only to this in-memory send attempt, never a persisted draft. */
export function reusableStagedAttachmentAttempt(
  attempt: StagedAttachmentAttempt | undefined,
  threadKey: string,
  sources: ReadonlyArray<AttachmentSource>,
  now: number,
): ReadonlyArray<StagedUploadChatAttachment> | null {
  if (
    !attempt ||
    attempt.threadKey !== threadKey ||
    now - attempt.createdAt >= 10 * 60_000 ||
    sources.length !== attempt.sources.length
  )
    return null;
  const unchanged = sources.every((source, index) => {
    const old = attempt.sources[index]!;
    return (
      source.file === old.file &&
      source.id === old.id &&
      source.type === old.type &&
      source.name === old.name &&
      source.mimeType === old.mimeType &&
      source.sizeBytes === old.sizeBytes
    );
  });
  return unchanged ? attempt.attachments : null;
}
