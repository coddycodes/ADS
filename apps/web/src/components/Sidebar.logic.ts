import * as React from "react";
import type { SidebarProjectSortOrder, SidebarThreadSortOrder } from "@coddy/contracts/settings";
import type { EnvironmentShellAvailability } from "@coddy/client-runtime/state/shell";
import type { ConnectionCatalogHealth } from "@coddy/client-runtime/platform";
import type { EnvironmentId, VcsStatusResult, VcsStatusSummary } from "@coddy/contracts";
import {
  getThreadSortTimestamp,
  sortThreads,
  toSortableTimestamp,
  type ThreadSortInput,
} from "../lib/threadSort";
import type { SidebarThreadSummary, Thread } from "../types";
import { scopeThreadRef, scopedThreadKey } from "@coddy/client-runtime/environment";
import { cn } from "../lib/utils";
import { KEYBOARD_CONTEXT_MENU_ECHO_MS } from "../contextMenuKeyboard";
import { isLatestTurnSettled } from "../session-logic";
import { resolveServerBackedAppStageLabel } from "../branding.logic";

export const THREAD_SELECTION_SAFE_SELECTOR = "[data-thread-item], [data-thread-selection-safe]";
export const THREAD_JUMP_HINT_SHOW_DELAY_MS = 100;
// Visible sidebar rows are prewarmed into the thread-detail cache so opening a
// nearby thread usually reuses an already-hot subscription.
export const SIDEBAR_THREAD_PREWARM_LIMIT = 10;
export type SidebarNewThreadEnvMode = "local" | "worktree";

export type SidebarProjectAvailabilityView =
  | {
      readonly kind: "available" | "empty-confirmed" | "loading";
      readonly environmentId: null;
      readonly error: null;
    }
  | {
      readonly kind:
        | "degraded"
        | "storage-changed"
        | "recovery-required"
        | "unavailable"
        | "configuration-error";
      readonly environmentId: EnvironmentId | null;
      readonly error: string | null;
      readonly hasCachedProjects: boolean;
    };

const SIDEBAR_PROJECT_AVAILABILITY_PRIORITY = [
  "recovery-required",
  "storage-changed",
  "configuration-error",
  "unavailable",
  "degraded",
  "synchronizing",
  "starting",
] as const;

export function resolveSidebarProjectAvailability(input: {
  readonly projectCount: number;
  readonly catalogReady: boolean;
  readonly catalogHealth?: ConnectionCatalogHealth;
  readonly environments: ReadonlyArray<EnvironmentShellAvailability>;
}): SidebarProjectAvailabilityView {
  if (input.catalogHealth?.status === "recovery-required") {
    return {
      kind: "configuration-error",
      environmentId: null,
      error: input.catalogHealth.message,
      hasCachedProjects: input.projectCount > 0,
    };
  }
  if (!input.catalogReady) {
    return { kind: "loading", environmentId: null, error: null };
  }

  const everyEnvironmentIsAuthoritative =
    input.environments.length > 0 &&
    input.environments.every(
      (environment) => environment.status === "live" && environment.hasSnapshot,
    );
  if (everyEnvironmentIsAuthoritative) {
    return input.projectCount === 0
      ? { kind: "empty-confirmed", environmentId: null, error: null }
      : { kind: "available", environmentId: null, error: null };
  }

  for (const status of SIDEBAR_PROJECT_AVAILABILITY_PRIORITY) {
    const environment = input.environments.find((candidate) => candidate.status === status);
    if (environment === undefined) {
      continue;
    }
    if (status === "starting" || status === "synchronizing") {
      return { kind: "loading", environmentId: null, error: null };
    }
    return {
      kind: status,
      environmentId: environment.environmentId,
      error: environment.error,
      hasCachedProjects: environment.hasSnapshot && input.projectCount > 0,
    };
  }

  return {
    kind: "unavailable",
    environmentId: null,
    error: null,
    hasCachedProjects: input.projectCount > 0,
  };
}
type SidebarProject = {
  id: string;
  title: string;
  createdAt?: string | undefined;
  updatedAt?: string | undefined;
};

export type ThreadTraversalDirection = "previous" | "next";

/**
 * Status tones shared by the thread pill (Agents view) and the card glyphs, so
 * both surfaces colour a status the same way.
 */
const STATUS_TONES = {
  approval: {
    colorClass: "text-amber-600 dark:text-amber-300/90",
    dotClass: "bg-amber-500 dark:bg-amber-300/90",
  },
  input: {
    colorClass: "text-indigo-600 dark:text-indigo-300/90",
    dotClass: "bg-indigo-500 dark:bg-indigo-300/90",
  },
  working: {
    colorClass: "text-sky-600 dark:text-sky-300/80",
    dotClass: "bg-sky-500 dark:bg-sky-300/80",
  },
  plan: {
    colorClass: "text-violet-600 dark:text-violet-300/90",
    dotClass: "bg-violet-500 dark:bg-violet-300/90",
  },
  completed: {
    colorClass: "text-emerald-600 dark:text-emerald-300/90",
    dotClass: "bg-emerald-500 dark:bg-emerald-300/90",
  },
} as const;

export interface ThreadStatusPill {
  label:
    | "Working"
    | "Connecting"
    | "Completed"
    | "Pending Approval"
    | "Awaiting Input"
    | "Plan Ready";
  colorClass: string;
  dotClass: string;
  pulse: boolean;
}

type ThreadStatusInput = Pick<
  SidebarThreadSummary,
  | "hasActionableProposedPlan"
  | "hasPendingApprovals"
  | "hasPendingUserInput"
  | "interactionMode"
  | "latestTurn"
  | "session"
> & {
  lastVisitedAt?: string | undefined;
};

export interface ThreadJumpHintVisibilityController {
  sync: (shouldShow: boolean) => void;
  dispose: () => void;
}

export function resolveSidebarStageBadgeLabel(input: {
  primaryServerVersion: string | null | undefined;
  fallbackStageLabel: string;
}): string | null {
  const stageLabel = resolveServerBackedAppStageLabel(input);
  return stageLabel === "Latest" ? null : stageLabel;
}

export function createThreadJumpHintVisibilityController(input: {
  delayMs: number;
  onVisibilityChange: (visible: boolean) => void;
  setTimeoutFn?: typeof globalThis.setTimeout;
  clearTimeoutFn?: typeof globalThis.clearTimeout;
}): ThreadJumpHintVisibilityController {
  const setTimeoutFn = input.setTimeoutFn ?? globalThis.setTimeout;
  const clearTimeoutFn = input.clearTimeoutFn ?? globalThis.clearTimeout;
  let isVisible = false;
  let timeoutId: NodeJS.Timeout | null = null;

  const clearPendingShow = () => {
    if (timeoutId === null) {
      return;
    }
    clearTimeoutFn(timeoutId);
    timeoutId = null;
  };

  return {
    sync: (shouldShow) => {
      if (!shouldShow) {
        clearPendingShow();
        if (isVisible) {
          isVisible = false;
          input.onVisibilityChange(false);
        }
        return;
      }

      if (isVisible || timeoutId !== null) {
        return;
      }

      timeoutId = setTimeoutFn(() => {
        timeoutId = null;
        isVisible = true;
        input.onVisibilityChange(true);
      }, input.delayMs);
    },
    dispose: () => {
      clearPendingShow();
    },
  };
}

export function useThreadJumpHintVisibility(): {
  showThreadJumpHints: boolean;
  updateThreadJumpHintsVisibility: (shouldShow: boolean) => void;
} {
  const [showThreadJumpHints, setShowThreadJumpHints] = React.useState(false);
  const controllerRef = React.useRef<ThreadJumpHintVisibilityController | null>(null);

  React.useEffect(() => {
    const controller = createThreadJumpHintVisibilityController({
      delayMs: THREAD_JUMP_HINT_SHOW_DELAY_MS,
      onVisibilityChange: (visible) => {
        setShowThreadJumpHints(visible);
      },
      setTimeoutFn: window.setTimeout.bind(window),
      clearTimeoutFn: window.clearTimeout.bind(window),
    });
    controllerRef.current = controller;

    return () => {
      controller.dispose();
      controllerRef.current = null;
    };
  }, []);

  const updateThreadJumpHintsVisibility = React.useCallback((shouldShow: boolean) => {
    controllerRef.current?.sync(shouldShow);
  }, []);

  return {
    showThreadJumpHints,
    updateThreadJumpHintsVisibility,
  };
}

export function hasUnseenCompletion(thread: ThreadStatusInput): boolean {
  if (!thread.latestTurn?.completedAt) return false;
  const completedAt = Date.parse(thread.latestTurn.completedAt);
  if (Number.isNaN(completedAt)) return false;
  if (!thread.lastVisitedAt) return false;

  const lastVisitedAt = Date.parse(thread.lastVisitedAt);
  if (Number.isNaN(lastVisitedAt)) return true;
  return completedAt > lastVisitedAt;
}

/**
 * The branch a workspace card shows: the VCS status's ref, else its detached
 * HEAD, else the branch the thread recorded. A stale passive summary still names
 * the branch: a failed PR lookup marks a just-read summary stale, while the
 * recorded branch lags checkouts made when its thread was closed.
 */
export function resolveWorkspaceBranchLabel(
  status: VcsStatusResult | VcsStatusSummary | null | undefined,
  fallbackBranch: string | null,
): string | null {
  const liveBranch = status
    ? (status.refName ?? ("detachedHead" in status ? status.detachedHead : null))
    : null;
  return liveBranch || fallbackBranch;
}

/**
 * Whether the checkout has uncommitted changes, per the latest status even when it
 * is stale: a failed PR lookup marks a just-read base stale, and hiding the dot
 * would claim the checkout is clean.
 */
export function resolveWorkspaceDirty(
  status: VcsStatusResult | VcsStatusSummary | null | undefined,
): boolean {
  return Boolean(status?.hasWorkingTreeChanges);
}

/** Line 2 hides the branch when it repeats the title (the primary card's title is its branch). */
export function shouldShowWorkspaceBranchText(
  branch: string | null,
  title: string,
): branch is string {
  return branch !== null && branch !== title;
}

export function shouldClearThreadSelectionOnMouseDown(target: HTMLElement | null): boolean {
  if (target === null) return true;
  return !target.closest(THREAD_SELECTION_SAFE_SELECTOR);
}

// A double-click dispatches two `click` events before `dblclick`: the first has
// `detail === 1`, the second `detail === 2`. The second click must not run the
// row's single-click navigation, otherwise double-click-to-rename would also
// navigate. `MouseEvent.detail` is 0 for synthetic/keyboard activations, which
// still count as a normal single activation.
export function isTrailingDoubleClick(detail: number): boolean {
  return detail > 1;
}

export function resolveSidebarNewThreadEnvMode(input: {
  requestedEnvMode?: SidebarNewThreadEnvMode;
  defaultEnvMode: SidebarNewThreadEnvMode;
}): SidebarNewThreadEnvMode {
  return input.requestedEnvMode ?? input.defaultEnvMode;
}

export function resolveSidebarNewThreadSeedContext(input: {
  projectId: string;
  defaultEnvMode: SidebarNewThreadEnvMode;
  activeThread?: {
    projectId: string;
    branch: string | null;
    worktreePath: string | null;
  } | null;
  activeDraftThread?: {
    projectId: string;
    branch: string | null;
    worktreePath: string | null;
    envMode: SidebarNewThreadEnvMode;
    startFromOrigin: boolean;
  } | null;
}): {
  branch?: string | null;
  worktreePath?: string | null;
  envMode: SidebarNewThreadEnvMode;
  startFromOrigin?: boolean;
} {
  if (input.defaultEnvMode === "worktree") {
    return {
      envMode: "worktree",
    };
  }

  if (input.activeDraftThread?.projectId === input.projectId) {
    return {
      branch: input.activeDraftThread.branch,
      worktreePath: input.activeDraftThread.worktreePath,
      envMode: input.activeDraftThread.envMode,
      startFromOrigin: input.activeDraftThread.startFromOrigin,
    };
  }

  if (input.activeThread?.projectId === input.projectId) {
    return {
      branch: input.activeThread.branch,
      worktreePath: input.activeThread.worktreePath,
      envMode: input.activeThread.worktreePath ? "worktree" : "local",
    };
  }

  return {
    envMode: input.defaultEnvMode,
  };
}

export function orderItemsByPreferredIds<TItem, TId>(input: {
  items: readonly TItem[];
  preferredIds: readonly TId[];
  getId: (item: TItem) => TId;
  getPreferenceIds?: (item: TItem) => readonly TId[];
}): TItem[] {
  const { getId, getPreferenceIds, items, preferredIds } = input;
  if (preferredIds.length === 0) {
    return [...items];
  }

  const indexesByPreferenceId = new Map<TId, number[]>();
  for (const [index, item] of items.entries()) {
    const preferenceIds = getPreferenceIds?.(item) ?? [getId(item)];
    for (const preferenceId of new Set(preferenceIds)) {
      const indexes = indexesByPreferenceId.get(preferenceId);
      if (indexes) {
        indexes.push(index);
      } else {
        indexesByPreferenceId.set(preferenceId, [index]);
      }
    }
  }

  const emittedIndexes = new Set<number>();
  const ordered = preferredIds.flatMap((id) => {
    const index = indexesByPreferenceId
      .get(id)
      ?.find((candidate) => !emittedIndexes.has(candidate));
    if (index === undefined) {
      return [];
    }
    emittedIndexes.add(index);
    return [items[index]!];
  });
  const remaining = items.filter((_, index) => !emittedIndexes.has(index));
  return [...ordered, ...remaining];
}

export function getVisibleSidebarThreadIds<TThreadId>(
  renderedProjects: readonly {
    shouldShowThreadPanel?: boolean;
    renderedThreadIds: readonly TThreadId[];
  }[],
): TThreadId[] {
  return renderedProjects.flatMap((renderedProject) =>
    renderedProject.shouldShowThreadPanel === false ? [] : renderedProject.renderedThreadIds,
  );
}

export function getSidebarThreadIdsToPrewarm<TThreadId>(
  visibleThreadIds: readonly TThreadId[],
  limit = SIDEBAR_THREAD_PREWARM_LIMIT,
): TThreadId[] {
  return visibleThreadIds.slice(0, Math.max(0, limit));
}

export function resolveAdjacentThreadId<T>(input: {
  threadIds: readonly T[];
  currentThreadId: T | null;
  direction: ThreadTraversalDirection;
}): T | null {
  const { currentThreadId, direction, threadIds } = input;

  if (threadIds.length === 0) {
    return null;
  }

  if (currentThreadId === null) {
    return direction === "previous" ? (threadIds.at(-1) ?? null) : (threadIds[0] ?? null);
  }

  const currentIndex = threadIds.indexOf(currentThreadId);
  if (currentIndex === -1) {
    return null;
  }

  if (direction === "previous") {
    return currentIndex > 0 ? (threadIds[currentIndex - 1] ?? null) : null;
  }

  return currentIndex < threadIds.length - 1 ? (threadIds[currentIndex + 1] ?? null) : null;
}

export function isContextMenuPointerDown(input: {
  button: number;
  ctrlKey: boolean;
  isMac: boolean;
}): boolean {
  if (input.button === 2) return true;
  return input.isMac && input.button === 0 && input.ctrlKey;
}

/** Where a menu opened from the keyboard or a button appears: the element's bottom-left. */
export function contextMenuAnchorForRect(rect: Pick<DOMRect, "left" | "bottom">): {
  x: number;
  y: number;
} {
  return { x: Math.round(rect.left), y: Math.round(rect.bottom) };
}

export function isWorkspaceThreadRunning(thread: Pick<ThreadStatusInput, "session">): boolean {
  return thread.session?.status === "running";
}

export function resolveThreadStatusPill(input: {
  thread: ThreadStatusInput;
}): ThreadStatusPill | null {
  const { thread } = input;

  if (thread.hasPendingApprovals) {
    return { label: "Pending Approval", ...STATUS_TONES.approval, pulse: false };
  }

  if (thread.hasPendingUserInput) {
    return { label: "Awaiting Input", ...STATUS_TONES.input, pulse: false };
  }

  if (isWorkspaceThreadRunning(thread)) {
    return { label: "Working", ...STATUS_TONES.working, pulse: true };
  }

  if (thread.session?.status === "starting") {
    return { label: "Connecting", ...STATUS_TONES.working, pulse: true };
  }

  const hasPlanReadyPrompt =
    !thread.hasPendingUserInput &&
    thread.interactionMode === "plan" &&
    isLatestTurnSettled(thread.latestTurn, thread.session) &&
    thread.hasActionableProposedPlan;
  if (hasPlanReadyPrompt) {
    return { label: "Plan Ready", ...STATUS_TONES.plan, pulse: false };
  }

  if (hasUnseenCompletion(thread)) {
    return { label: "Completed", ...STATUS_TONES.completed, pulse: false };
  }

  return null;
}

export type WorkspaceCardStatusKind =
  | "approval"
  | "input"
  | "working"
  | "failed"
  | "plan"
  | "done"
  | "idle";

export interface WorkspaceCardStatus {
  readonly kind: WorkspaceCardStatusKind;
  /** Accessible name and tooltip of the glyph. */
  readonly label: string;
  readonly colorClass: string;
}

/**
 * The card glyph states from the spec's status table. `resolveWorkspaceCardStatus`
 * returns these exact objects, so memoised components compare them by identity.
 */
export const WORKSPACE_CARD_STATUS = {
  approval: {
    kind: "approval",
    label: "Needs approval",
    colorClass: STATUS_TONES.approval.colorClass,
  },
  input: {
    kind: "input",
    label: "Waiting for your answer",
    colorClass: STATUS_TONES.input.colorClass,
  },
  working: { kind: "working", label: "Working", colorClass: STATUS_TONES.working.colorClass },
  connecting: { kind: "working", label: "Connecting", colorClass: STATUS_TONES.working.colorClass },
  failed: { kind: "failed", label: "Failed", colorClass: "text-destructive" },
  plan: { kind: "plan", label: "Plan ready", colorClass: STATUS_TONES.plan.colorClass },
  done: {
    kind: "done",
    label: "Finished, not opened yet",
    colorClass: STATUS_TONES.completed.colorClass,
  },
  idle: { kind: "idle", label: "Idle", colorClass: "text-muted-foreground" },
} as const satisfies Record<string, WorkspaceCardStatus>;

export type WorkspaceCardStatusInput = Omit<ThreadStatusInput, "lastVisitedAt"> &
  Pick<SidebarThreadSummary, "unresolvedDelivery">;

/**
 * A card's glyph, wrapping the thread pill so the Agents view keeps its labels.
 * First match wins: approval, input, working/connecting, failed (session error,
 * refused delivery, or an unseen turn that errored — the pill calls that
 * "Completed"), plan ready, finished-not-opened, idle.
 */
export function resolveWorkspaceCardStatus(
  thread: WorkspaceCardStatusInput,
  lastVisitedAt: string | null | undefined,
): WorkspaceCardStatus {
  const visited: ThreadStatusInput = lastVisitedAt ? { ...thread, lastVisitedAt } : thread;
  const pill = resolveThreadStatusPill({ thread: visited });
  switch (pill?.label) {
    case "Pending Approval":
      return WORKSPACE_CARD_STATUS.approval;
    case "Awaiting Input":
      return WORKSPACE_CARD_STATUS.input;
    case "Working":
      return WORKSPACE_CARD_STATUS.working;
    case "Connecting":
      return WORKSPACE_CARD_STATUS.connecting;
    default:
      break;
  }
  if (
    thread.session?.status === "error" ||
    thread.unresolvedDelivery?.state === "failed" ||
    (thread.latestTurn?.state === "error" && hasUnseenCompletion(visited))
  ) {
    return WORKSPACE_CARD_STATUS.failed;
  }
  if (pill?.label === "Plan Ready") {
    return WORKSPACE_CARD_STATUS.plan;
  }
  if (pill?.label === "Completed") {
    return WORKSPACE_CARD_STATUS.done;
  }
  return WORKSPACE_CARD_STATUS.idle;
}

const WORKSPACE_CARD_STATUS_PRIORITY: Record<WorkspaceCardStatusKind, number> = {
  approval: 7,
  input: 6,
  working: 5,
  failed: 4,
  plan: 3,
  done: 2,
  idle: 1,
};

/** The more urgent of two statuses; the first wins ties. */
export function pickMoreUrgentWorkspaceCardStatus(
  left: WorkspaceCardStatus,
  right: WorkspaceCardStatus,
): WorkspaceCardStatus {
  return WORKSPACE_CARD_STATUS_PRIORITY[right.kind] > WORKSPACE_CARD_STATUS_PRIORITY[left.kind]
    ? right
    : left;
}

/**
 * The most urgent non-idle status, for summaries of hidden cards (a collapsed
 * project, the Show more row). Null when every card is idle.
 */
export function resolveHighestWorkspaceCardStatus(
  statuses: Iterable<WorkspaceCardStatus>,
): WorkspaceCardStatus | null {
  let highest: WorkspaceCardStatus | null = null;
  for (const status of statuses) {
    if (status.kind === "idle") continue;
    highest = highest === null ? status : pickMoreUrgentWorkspaceCardStatus(highest, status);
  }
  return highest;
}

export function createWorkspaceStatusLookup(
  threads: readonly SidebarThreadSummary[],
  visits: readonly (string | null | undefined)[],
) {
  const visitsByKey = new Map(
    threads.map(
      (thread, index) =>
        [
          scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)),
          visits[index] ?? null,
        ] as const,
    ),
  );
  const lastVisitedAt = (thread: SidebarThreadSummary) =>
    visitsByKey.get(scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id)));
  const statusOf = (thread: SidebarThreadSummary) =>
    resolveWorkspaceCardStatus(thread, lastVisitedAt(thread));
  return { lastVisitedAt, statusOf };
}

type WorkspaceChatThread = WorkspaceCardStatusInput & {
  readonly environmentId: string;
  readonly projectId: string;
  readonly kind?: "default" | "workspace" | "panel" | undefined;
  readonly worktreePath: string | null;
  readonly archivedAt: string | null;
};

export interface WorkspaceChatSummary {
  readonly count: number;
  /** The most urgent status among the chats, idle included; shown as the row's glyph. */
  readonly status: WorkspaceCardStatus;
}

/**
 * The checkout a chat runs in. Panels copy their host's worktree path, so a
 * worktree card matches on it; a panel without a path runs in its project's main
 * checkout and counts on the primary card.
 */
export function workspaceCheckoutKey(input: {
  readonly environmentId: string;
  readonly projectId: string;
  readonly worktreePath: string | null;
}): string {
  return input.worktreePath === null
    ? `main\u0000${input.environmentId}\u0000${input.projectId}`
    : `worktree\u0000${input.environmentId}\u0000${input.worktreePath}`;
}

/** Counts unarchived panel chats per checkout with their most urgent status. O(threads). */
export function summarizeWorkspaceChats<T extends WorkspaceChatThread>(
  threads: readonly T[],
  resolveLastVisitedAt: (thread: T) => string | null | undefined,
): ReadonlyMap<string, WorkspaceChatSummary> {
  const summaries = new Map<string, { count: number; status: WorkspaceCardStatus }>();
  for (const thread of threads) {
    if (thread.kind !== "panel" || thread.archivedAt !== null) continue;
    const key = workspaceCheckoutKey(thread);
    const status = resolveWorkspaceCardStatus(thread, resolveLastVisitedAt(thread));
    const current = summaries.get(key);
    if (current === undefined) {
      summaries.set(key, { count: 1, status });
    } else {
      current.count += 1;
      current.status = pickMoreUrgentWorkspaceCardStatus(current.status, status);
    }
  }
  return summaries;
}

type WorktreeSessionThread = Pick<
  WorkspaceChatThread,
  "environmentId" | "projectId" | "worktreePath" | "archivedAt"
> &
  Pick<ThreadStatusInput, "session">;

/**
 * Whether a provider session is running in a worktree card's checkout: the
 * card's own thread, or an unarchived chat open in the same worktree (its
 * "N more chats"). Deletion waits for both `running` and `starting` sessions,
 * matching the server's removal refusal. The Working pill still uses
 * `isWorkspaceThreadRunning` (session status `running`).
 *
 * Archived chats are skipped, as in `summarizeWorkspaceChats`, even though
 * archiving does not stop a session (the engine only records `thread.archived`),
 * so an archived chat can leave an idle session in the worktree. That is safe:
 * archiving requires no active turn, the sidebar offers no way to stop a chat it
 * hides, and removing the worktree stops any idle session left behind (the
 * server's `stop_session_if_current` in `worktree_runtime.rs`).
 *
 * O(threads), so run it when a menu opens or memoize it, never on every render.
 * A thread without a worktree has none.
 */
export function isWorktreeSessionRunning<T extends WorktreeSessionThread>(
  card: T,
  threads: Iterable<T>,
): boolean {
  if (card.worktreePath === null) return false;
  if (isWorkspaceThreadRunning(card) || card.session?.status === "starting") return true;
  const key = workspaceCheckoutKey(card);
  for (const thread of threads) {
    if (
      thread.archivedAt === null &&
      (isWorkspaceThreadRunning(thread) || thread.session?.status === "starting") &&
      workspaceCheckoutKey(thread) === key
    ) {
      return true;
    }
  }
  return false;
}

/**
 * A card's surface for its state, per States.dc.html, refined by the user on
 * 2026-09-25: every card is outlined so neighbouring cards read as separate.
 * Idle cards use the standard border token, the active card a stronger neutral
 * border, and multi-selected cards a tinted one; the orange ring stays for focus.
 */
/** The lifted surface of one project (or repository) in the left panel's list well. */
export const SIDEBAR_CARD_CLASS =
  "rounded-[10px] border border-sidebar-card-border bg-sidebar-card shadow-sidebar-card";

export function resolveWorkspaceCardClassName(input: {
  isActive: boolean;
  isSelected: boolean;
}): string {
  const base = "w-full rounded-md border select-none";
  if (input.isSelected && input.isActive) {
    return cn(
      base,
      "border-primary/60 bg-primary/22 hover:bg-primary/26 dark:bg-primary/30 dark:hover:bg-primary/36",
    );
  }
  if (input.isSelected) {
    return cn(
      base,
      "border-primary/40 bg-primary/15 hover:bg-primary/19 dark:bg-primary/22 dark:hover:bg-primary/28",
    );
  }
  // Chats are rows on their project's card, so only selection draws an outline.
  if (input.isActive) {
    return cn(base, "border-transparent bg-foreground/8 dark:bg-foreground/12");
  }
  return cn(base, "border-transparent hover:bg-accent/60");
}

/** Shift+F10 or the Menu key: the platform shortcuts for a focused element's menu. */
export function isContextMenuShortcut(
  event: Pick<KeyboardEvent, "key" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey">,
): boolean {
  if (event.key === "ContextMenu") {
    return true;
  }
  return event.key === "F10" && event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey;
}

/**
 * Chromium and WebView2 follow Shift+F10 and the Menu key with a contextmenu
 * event; one arriving within this window after a keyboard-opened menu is ignored.
 */
export function isKeyboardContextMenuEcho(openedAtMs: number, nowMs: number): boolean {
  const elapsedMs = nowMs - openedAtMs;
  return elapsedMs >= 0 && elapsedMs < KEYBOARD_CONTEXT_MENU_ECHO_MS;
}

export function getVisibleThreadsForProject<T extends Pick<Thread, "id">>(input: {
  threads: readonly T[];
  activeThreadId: T["id"] | undefined;
  isThreadListExpanded: boolean;
  previewLimit: number;
}): {
  hasHiddenThreads: boolean;
  visibleThreads: T[];
  hiddenThreads: T[];
} {
  const { activeThreadId, isThreadListExpanded, previewLimit, threads } = input;
  const hasHiddenThreads = threads.length > previewLimit;

  if (!hasHiddenThreads || isThreadListExpanded) {
    return {
      hasHiddenThreads,
      hiddenThreads: [],
      visibleThreads: [...threads],
    };
  }

  const previewThreads = threads.slice(0, previewLimit);
  if (!activeThreadId || previewThreads.some((thread) => thread.id === activeThreadId)) {
    return {
      hasHiddenThreads: true,
      hiddenThreads: threads.slice(previewLimit),
      visibleThreads: previewThreads,
    };
  }

  const activeThread = threads.find((thread) => thread.id === activeThreadId);
  if (!activeThread) {
    return {
      hasHiddenThreads: true,
      hiddenThreads: threads.slice(previewLimit),
      visibleThreads: previewThreads,
    };
  }

  const visibleThreadIds = new Set([...previewThreads, activeThread].map((thread) => thread.id));

  return {
    hasHiddenThreads: true,
    hiddenThreads: threads.filter((thread) => !visibleThreadIds.has(thread.id)),
    visibleThreads: threads.filter((thread) => visibleThreadIds.has(thread.id)),
  };
}

export function getFallbackThreadIdAfterDelete<
  T extends Pick<Thread, "id" | "projectId" | "createdAt" | "updatedAt"> & ThreadSortInput,
>(input: {
  threads: readonly T[];
  deletedThreadId: T["id"];
  sortOrder: SidebarThreadSortOrder;
  deletedThreadIds?: ReadonlySet<T["id"]>;
}): T["id"] | null {
  const { deletedThreadId, deletedThreadIds, sortOrder, threads } = input;
  const deletedThread = threads.find((thread) => thread.id === deletedThreadId);
  if (!deletedThread) {
    return null;
  }

  return (
    sortThreads(
      threads.filter(
        (thread) =>
          thread.projectId === deletedThread.projectId &&
          thread.id !== deletedThreadId &&
          !deletedThreadIds?.has(thread.id),
      ),
      sortOrder,
    )[0]?.id ?? null
  );
}
export function getProjectSortTimestamp(
  project: SidebarProject,
  projectThreads: readonly ThreadSortInput[],
  sortOrder: Exclude<SidebarProjectSortOrder, "manual">,
): number {
  if (projectThreads.length > 0) {
    return projectThreads.reduce(
      (latest, thread) => Math.max(latest, getThreadSortTimestamp(thread, sortOrder)),
      Number.NEGATIVE_INFINITY,
    );
  }

  if (sortOrder === "created_at") {
    return toSortableTimestamp(project.createdAt) ?? Number.NEGATIVE_INFINITY;
  }
  return toSortableTimestamp(project.updatedAt ?? project.createdAt) ?? Number.NEGATIVE_INFINITY;
}

// --- Orca-parity workspace-row helpers (primary row + pin ordering + duration) ---

export interface WorkspaceRowThreadInput {
  readonly kind?: "default" | "workspace" | "panel" | undefined;
}

/** Finds the project's default (primary-row) thread, if the server has backfilled one. */
export function findDefaultThread<T extends WorkspaceRowThreadInput>(
  threads: readonly T[],
): T | null {
  return threads.find((thread) => thread.kind === "default") ?? null;
}

/**
 * Splits a project's threads into the primary-row thread (`kind: "default"`)
 * and the remaining workspace rows (worktree/ad-hoc threads). `kind: "panel"`
 * threads are center-panel siblings of a host thread and never appear as
 * sidebar rows, so they are excluded from `workspaceThreads` entirely. `null`
 * primary means the server hasn't backfilled a default thread yet for this
 * project; callers fall back to creating one on click.
 */
export function splitPrimaryAndWorkspaceThreads<T extends WorkspaceRowThreadInput>(
  threads: readonly T[],
): { primaryThread: T | null; workspaceThreads: T[] } {
  const primaryThread = findDefaultThread(threads);
  const workspaceThreads = threads.filter(
    (thread) => thread !== primaryThread && thread.kind !== "panel",
  );
  return { primaryThread, workspaceThreads };
}

/**
 * Stable-partitions `items` so pinned entries (per `sidebarWorkspaceMetaStore`)
 * come first, preserving the caller's existing relative order (typically
 * already sorted via `sortThreads`) within each partition.
 */
export function orderRowsWithPins<T>(
  items: readonly T[],
  pinnedKeys: ReadonlySet<string> | readonly string[],
  getKey: (item: T) => string,
): T[] {
  const pinned = pinnedKeys instanceof Set ? pinnedKeys : new Set(pinnedKeys);
  const pinnedItems: T[] = [];
  const restItems: T[] = [];
  for (const item of items) {
    (pinned.has(getKey(item)) ? pinnedItems : restItems).push(item);
  }
  return [...pinnedItems, ...restItems];
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/**
 * The instant a card's age counts from: the running turn's start while the agent
 * works, else the latest user message, update or creation, as the rows did.
 */
export function resolveWorkspaceCardAgeSource(
  thread: Pick<
    SidebarThreadSummary,
    "latestTurn" | "latestUserMessageAt" | "updatedAt" | "createdAt"
  >,
  isWorking: boolean,
): string | null {
  if (isWorking && thread.latestTurn?.startedAt) {
    return thread.latestTurn.startedAt;
  }
  return thread.latestUserMessageAt ?? thread.updatedAt ?? thread.createdAt ?? null;
}

/** A compact age for card line 3: now, 5m, 3h, 2d. Null when the instant is unusable. */
export function formatCompactAge(iso: string | null | undefined, now: number): string | null {
  if (!iso) return null;
  const startedAt = Date.parse(iso);
  if (Number.isNaN(startedAt)) return null;
  const elapsedMs = Math.max(0, now - startedAt);
  if (elapsedMs < MINUTE_MS) return "now";
  if (elapsedMs < HOUR_MS) return `${Math.floor(elapsedMs / MINUTE_MS)}m`;
  if (elapsedMs < DAY_MS) return `${Math.floor(elapsedMs / HOUR_MS)}h`;
  return `${Math.floor(elapsedMs / DAY_MS)}d`;
}

export function sortProjectsForSidebar<
  TProject extends SidebarProject,
  TThread extends Pick<Thread, "projectId" | "createdAt" | "updatedAt"> & ThreadSortInput,
>(
  projects: readonly TProject[],
  threads: readonly TThread[],
  sortOrder: SidebarProjectSortOrder,
): TProject[] {
  if (sortOrder === "manual") {
    return [...projects];
  }

  const threadsByProjectId = new Map<string, TThread[]>();
  for (const thread of threads) {
    const existing = threadsByProjectId.get(thread.projectId) ?? [];
    existing.push(thread);
    threadsByProjectId.set(thread.projectId, existing);
  }

  return [...projects].toSorted((left, right) => {
    const rightTimestamp = getProjectSortTimestamp(
      right,
      threadsByProjectId.get(right.id) ?? [],
      sortOrder,
    );
    const leftTimestamp = getProjectSortTimestamp(
      left,
      threadsByProjectId.get(left.id) ?? [],
      sortOrder,
    );
    const byTimestamp =
      rightTimestamp === leftTimestamp ? 0 : rightTimestamp > leftTimestamp ? 1 : -1;
    if (byTimestamp !== 0) return byTimestamp;
    return left.title.localeCompare(right.title) || left.id.localeCompare(right.id);
  });
}
