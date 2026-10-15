import {
  parseScopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
  scopedThreadKey,
} from "@bibcode/client-runtime/environment";
import { settlePromise } from "@bibcode/client-runtime/state/runtime";
import {
  EnvironmentId,
  type ScopedProjectRef,
  type ScopedThreadRef,
  ThreadId,
  type WorktreeRemovalResult,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import { AsyncResult } from "effect/unstable/reactivity";
import { useRouter } from "@tanstack/react-router";
import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";

import { getFallbackThreadIdAfterDelete } from "../components/Sidebar.logic";
import { useCenterPanelStore } from "../centerPanelStore";
import { useComposerDraftStore } from "../composerDraftStore";
import { terminalEnvironment } from "../state/terminal";
import { threadEnvironment } from "../state/threads";
import { worktreeEnvironment } from "../state/worktrees";
import { useNewThreadHandler } from "./useHandleNewThread";
import { refreshArchivedThreadsForEnvironment } from "../lib/archivedThreadsState";
import { newCommandId } from "../lib/utils";
import { readLocalApi } from "../localApi";
import { readEnvironmentThreadRefs, readThreadShell } from "../state/entities";
import { useRightPanelStore } from "../rightPanelStore";
import { buildThreadRouteParams, resolveThreadRouteRef } from "../threadRoutes";
import { stackedThreadToast, toastManager } from "../components/ui/toast";
import { useClientSettings } from "./useSettings";
import { useAtomCommand } from "../state/use-atom-command";
import type { WorktreeRemovalTarget } from "../components/WorktreeRemovalDialog";

export class ThreadArchiveBlockedError extends Schema.TaggedError<ThreadArchiveBlockedError>()(
  "ThreadArchiveBlockedError",
  {
    environmentId: EnvironmentId,
    threadId: ThreadId,
  },
) {
  override get message(): string {
    return "Cannot archive a running thread.";
  }
}

function removeThreadPanelState(threadRef: ScopedThreadRef): void {
  useCenterPanelStore.getState().removeThread(threadRef);
  useRightPanelStore.getState().removeThread(threadRef);
}

interface WorktreeRemovalCleanupContext {
  readonly removed: ReadonlyArray<{
    readonly threadRef: ScopedThreadRef;
    readonly projectRef: ScopedProjectRef;
  }>;
  readonly removedIds: ReadonlySet<ThreadId>;
  readonly fallbackThreadRef: ScopedThreadRef | null;
}

export function useThreadActions() {
  const [worktreeRemovalTarget, setWorktreeRemovalTarget] = useState<WorktreeRemovalTarget | null>(
    null,
  );
  const closeTerminal = useAtomCommand(terminalEnvironment.close);
  const archiveThreadMutation = useAtomCommand(threadEnvironment.archive, {
    reportFailure: false,
  });
  const unarchiveThreadMutation = useAtomCommand(threadEnvironment.unarchive, {
    reportFailure: false,
  });
  const deleteThreadMutation = useAtomCommand(threadEnvironment.delete, {
    reportFailure: false,
  });
  const stopThreadSession = useAtomCommand(threadEnvironment.stopSession);
  const removeWorktreeFromBibCode = useAtomCommand(worktreeEnvironment.removeFromBibCode, {
    reportFailure: false,
  });
  const sidebarThreadSortOrder = useClientSettings((settings) => settings.sidebarThreadSortOrder);
  const confirmThreadDelete = useClientSettings((settings) => settings.confirmThreadDelete);
  const clearComposerDraftForThread = useComposerDraftStore((store) => store.clearDraftThread);
  const clearProjectDraftThreadById = useComposerDraftStore(
    (store) => store.clearProjectDraftThreadById,
  );
  const router = useRouter();
  const handleNewThread = useNewThreadHandler();
  // Keep a ref so archiveThread can call handleNewThread without appearing in
  // its dependency array — handleNewThread is inherently unstable (depends on
  // the projects list) and would otherwise cascade new references into every
  // sidebar row via archiveThread → attemptArchiveThread.
  const handleNewThreadRef = useRef(handleNewThread);
  const worktreeRemovalCleanupByThreadKeyRef = useRef(
    new Map<string, WorktreeRemovalCleanupContext>(),
  );
  useLayoutEffect(() => {
    handleNewThreadRef.current = handleNewThread;
  }, [handleNewThread]);

  const resolveThreadTarget = useCallback((target: ScopedThreadRef) => {
    const thread = readThreadShell(target);
    if (!thread) {
      return null;
    }
    return {
      thread,
      threadRef: target,
    };
  }, []);
  const getCurrentRouteThreadRef = useCallback(() => {
    const currentRouteParams = router.state.matches[router.state.matches.length - 1]?.params ?? {};
    return resolveThreadRouteRef(currentRouteParams);
  }, [router]);

  const captureWorktreeRemovalCleanup = useCallback(
    (target: WorktreeRemovalTarget): WorktreeRemovalCleanupContext => {
      const targetRef = scopeThreadRef(target.environmentId, target.threadId);
      const thread = readThreadShell(targetRef);
      const threads = readEnvironmentThreadRefs(target.environmentId).flatMap((ref) => {
        const shell = readThreadShell(ref);
        return shell === null ? [] : [shell];
      });
      const dependentPanels = thread
        ? threads.filter(
            (candidate) =>
              candidate.kind === "panel" && candidate.worktreePath === thread.worktreePath,
          )
        : [];
      const removed = [
        {
          threadRef: targetRef,
          projectRef: scopeProjectRef(target.environmentId, target.projectId),
        },
        ...dependentPanels
          .filter((candidate) => candidate.id !== target.threadId)
          .map((candidate) => ({
            threadRef: scopeThreadRef(target.environmentId, candidate.id),
            projectRef: scopeProjectRef(target.environmentId, candidate.projectId),
          })),
      ];
      const removedIds = new Set(removed.map(({ threadRef }) => threadRef.threadId));
      const fallbackThreadId = thread
        ? getFallbackThreadIdAfterDelete({
            threads,
            deletedThreadId: target.threadId,
            deletedThreadIds: removedIds,
            sortOrder: sidebarThreadSortOrder,
          })
        : null;
      const fallbackThread = fallbackThreadId
        ? threads.find((candidate) => candidate.id === fallbackThreadId)
        : null;
      const cleanup: WorktreeRemovalCleanupContext = {
        removed,
        removedIds,
        fallbackThreadRef: fallbackThread
          ? scopeThreadRef(fallbackThread.environmentId, fallbackThread.id)
          : null,
      };
      worktreeRemovalCleanupByThreadKeyRef.current.set(scopedThreadKey(targetRef), cleanup);
      return cleanup;
    },
    [sidebarThreadSortOrder],
  );

  const completeWorktreeRemoval = useCallback(
    async (target: WorktreeRemovalTarget, result: WorktreeRemovalResult) => {
      const targetRef = scopeThreadRef(target.environmentId, target.threadId);
      const cleanupKey = scopedThreadKey(targetRef);
      const cleanup =
        worktreeRemovalCleanupByThreadKeyRef.current.get(cleanupKey) ??
        captureWorktreeRemovalCleanup(target);
      worktreeRemovalCleanupByThreadKeyRef.current.delete(cleanupKey);
      const currentRouteThreadRef = getCurrentRouteThreadRef();
      const shouldNavigateToFallback =
        currentRouteThreadRef?.environmentId === target.environmentId &&
        cleanup.removedIds.has(currentRouteThreadRef.threadId);

      for (const removed of cleanup.removed) {
        clearComposerDraftForThread(removed.threadRef);
        clearProjectDraftThreadById(removed.projectRef, removed.threadRef);
        removeThreadPanelState(removed.threadRef);
      }
      refreshArchivedThreadsForEnvironment(target.environmentId);

      if (result.gitOutcome === "failed" || result.orphanCleanupPending) {
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: "Removed from BiBCode; Git cleanup remains",
            description:
              result.detail ??
              "The workspace row was removed, but Git may still need manual cleanup.",
          }),
        );
      }

      if (!shouldNavigateToFallback) {
        return AsyncResult.success(undefined);
      }
      return settlePromise(() =>
        cleanup.fallbackThreadRef
          ? router.navigate({
              to: "/$environmentId/$threadId",
              params: buildThreadRouteParams(cleanup.fallbackThreadRef),
              replace: true,
            })
          : router.navigate({ to: "/", replace: true }),
      );
    },
    [
      clearComposerDraftForThread,
      clearProjectDraftThreadById,
      captureWorktreeRemovalCleanup,
      getCurrentRouteThreadRef,
      router,
    ],
  );

  const requestWorktreeRemoval = useCallback(
    (target: WorktreeRemovalTarget) => {
      captureWorktreeRemovalCleanup(target);
      setWorktreeRemovalTarget(target);
    },
    [captureWorktreeRemovalCleanup],
  );
  const closeWorktreeRemovalDialog = useCallback(() => {
    setWorktreeRemovalTarget((current) => {
      if (current) {
        worktreeRemovalCleanupByThreadKeyRef.current.delete(
          scopedThreadKey(scopeThreadRef(current.environmentId, current.threadId)),
        );
      }
      return null;
    });
  }, []);

  const archiveThread = useCallback(
    async (target: ScopedThreadRef) => {
      const resolved = resolveThreadTarget(target);
      if (!resolved) return AsyncResult.success(undefined);
      const { thread, threadRef } = resolved;
      if (thread.session?.status === "running" && thread.session.activeTurnId != null) {
        return AsyncResult.failure(
          Cause.fail(
            new ThreadArchiveBlockedError({
              environmentId: threadRef.environmentId,
              threadId: threadRef.threadId,
            }),
          ),
        );
      }

      const currentRouteThreadRef = getCurrentRouteThreadRef();
      const shouldNavigateToDraft =
        currentRouteThreadRef?.threadId === threadRef.threadId &&
        currentRouteThreadRef.environmentId === threadRef.environmentId;
      const archiveResult = await archiveThreadMutation({
        environmentId: threadRef.environmentId,
        input: { threadId: threadRef.threadId },
      });
      if (archiveResult._tag === "Failure") {
        return archiveResult;
      }

      if (shouldNavigateToDraft) {
        const navigationResult = await settlePromise(() =>
          handleNewThreadRef.current(scopeProjectRef(thread.environmentId, thread.projectId)),
        );
        if (navigationResult._tag === "Failure") {
          return navigationResult;
        }
        refreshArchivedThreadsForEnvironment(threadRef.environmentId);
        return archiveResult;
      }

      refreshArchivedThreadsForEnvironment(threadRef.environmentId);
      return archiveResult;
    },
    [archiveThreadMutation, getCurrentRouteThreadRef, resolveThreadTarget],
  );

  const unarchiveThread = useCallback(
    async (target: ScopedThreadRef) => {
      const result = await unarchiveThreadMutation({
        environmentId: target.environmentId,
        input: { threadId: target.threadId },
      });
      if (result._tag === "Success") {
        refreshArchivedThreadsForEnvironment(target.environmentId);
      }
      return result;
    },
    [unarchiveThreadMutation],
  );

  const deleteThread = useCallback(
    async (target: ScopedThreadRef, opts: { deletedThreadKeys?: ReadonlySet<string> } = {}) => {
      const resolved = resolveThreadTarget(target);
      if (!resolved) {
        // Thread not in main store (e.g. archived thread) — dispatch delete directly.
        const result = await deleteThreadMutation({
          environmentId: target.environmentId,
          input: { threadId: target.threadId },
        });
        if (result._tag === "Success") {
          refreshArchivedThreadsForEnvironment(target.environmentId);
          removeThreadPanelState(target);
        }
        return result;
      }
      const { thread, threadRef } = resolved;
      const threads = readEnvironmentThreadRefs(threadRef.environmentId).flatMap((ref) => {
        const shell = readThreadShell(ref);
        return shell === null ? [] : [shell];
      });
      if (thread.worktreePath && thread.kind !== "panel") {
        const removalTarget: WorktreeRemovalTarget = {
          environmentId: threadRef.environmentId,
          projectId: thread.projectId,
          threadId: threadRef.threadId,
          title: thread.title,
          path: thread.worktreePath,
          branch: thread.branch,
          availability: "verification-unavailable",
          registrationState: null,
          locked: false,
        };
        captureWorktreeRemovalCleanup(removalTarget);
        const worktreeResult = await removeWorktreeFromBibCode({
          environmentId: threadRef.environmentId,
          input: {
            commandId: newCommandId(),
            projectId: thread.projectId,
            threadId: threadRef.threadId,
          },
        });
        if (worktreeResult._tag === "Failure") {
          worktreeRemovalCleanupByThreadKeyRef.current.delete(scopedThreadKey(threadRef));
          return worktreeResult;
        }

        const cleanupResult = await completeWorktreeRemoval(removalTarget, worktreeResult.value);
        if (cleanupResult._tag === "Failure") {
          return cleanupResult;
        }
        return worktreeResult;
      }
      const deletedIds =
        opts.deletedThreadKeys && opts.deletedThreadKeys.size > 0
          ? new Set<ThreadId>(
              [...opts.deletedThreadKeys].flatMap((threadKey) => {
                const ref = parseScopedThreadKey(threadKey);
                return ref && ref.environmentId === threadRef.environmentId ? [ref.threadId] : [];
              }),
            )
          : undefined;
      const dependentPanelThreads: typeof threads = [];
      const threadsToTeardown = [thread, ...dependentPanelThreads];
      for (const threadToTeardown of threadsToTeardown) {
        if (threadToTeardown.session && threadToTeardown.session.status !== "stopped") {
          const stopResult = await stopThreadSession({
            environmentId: threadRef.environmentId,
            input: { threadId: threadToTeardown.id },
          });
          if (stopResult._tag === "Failure") {
            return stopResult;
          }
        }

        const closeResult = await closeTerminal({
          environmentId: threadRef.environmentId,
          input: { threadId: threadToTeardown.id, deleteHistory: true },
        });
        if (closeResult._tag === "Failure") {
          return closeResult;
        }
      }

      const deletedThreadIds = new Set(deletedIds ?? []);
      for (const dependentPanelThread of dependentPanelThreads) {
        deletedThreadIds.add(dependentPanelThread.id);
      }
      const currentRouteThreadRef = getCurrentRouteThreadRef();
      const shouldNavigateToFallback =
        currentRouteThreadRef?.environmentId === threadRef.environmentId &&
        (currentRouteThreadRef.threadId === threadRef.threadId ||
          deletedThreadIds.has(currentRouteThreadRef.threadId));
      const fallbackThreadId = getFallbackThreadIdAfterDelete({
        threads,
        deletedThreadId: threadRef.threadId,
        deletedThreadIds,
        sortOrder: sidebarThreadSortOrder,
      });
      for (const dependentPanelThread of dependentPanelThreads) {
        const dependentPanelRef = scopeThreadRef(threadRef.environmentId, dependentPanelThread.id);
        const dependentDeleteResult = await deleteThreadMutation({
          environmentId: threadRef.environmentId,
          input: { threadId: dependentPanelThread.id },
        });
        if (dependentDeleteResult._tag === "Failure") {
          return dependentDeleteResult;
        }
        clearComposerDraftForThread(dependentPanelRef);
        clearProjectDraftThreadById(
          scopeProjectRef(threadRef.environmentId, dependentPanelThread.projectId),
          dependentPanelRef,
        );
        removeThreadPanelState(dependentPanelRef);
      }
      const deleteResult = await deleteThreadMutation({
        environmentId: threadRef.environmentId,
        input: { threadId: threadRef.threadId },
      });
      if (deleteResult._tag === "Failure") {
        return deleteResult;
      }
      refreshArchivedThreadsForEnvironment(threadRef.environmentId);
      clearComposerDraftForThread(threadRef);
      clearProjectDraftThreadById(
        scopeProjectRef(threadRef.environmentId, thread.projectId),
        threadRef,
      );
      removeThreadPanelState(threadRef);

      if (shouldNavigateToFallback) {
        if (fallbackThreadId) {
          const fallbackThread = readThreadShell(
            scopeThreadRef(threadRef.environmentId, fallbackThreadId),
          );
          if (fallbackThread) {
            const navigationResult = await settlePromise(() =>
              router.navigate({
                to: "/$environmentId/$threadId",
                params: buildThreadRouteParams(
                  scopeThreadRef(fallbackThread.environmentId, fallbackThread.id),
                ),
                replace: true,
              }),
            );
            if (navigationResult._tag === "Failure") {
              return navigationResult;
            }
          } else {
            const navigationResult = await settlePromise(() =>
              router.navigate({ to: "/", replace: true }),
            );
            if (navigationResult._tag === "Failure") {
              return navigationResult;
            }
          }
        } else {
          const navigationResult = await settlePromise(() =>
            router.navigate({ to: "/", replace: true }),
          );
          if (navigationResult._tag === "Failure") {
            return navigationResult;
          }
        }
      }

      return deleteResult;
    },
    [
      clearComposerDraftForThread,
      clearProjectDraftThreadById,
      captureWorktreeRemovalCleanup,
      closeTerminal,
      completeWorktreeRemoval,
      deleteThreadMutation,
      getCurrentRouteThreadRef,
      removeWorktreeFromBibCode,
      router,
      resolveThreadTarget,
      sidebarThreadSortOrder,
      stopThreadSession,
    ],
  );

  const confirmAndDeleteThread = useCallback(
    async (target: ScopedThreadRef) => {
      const localApi = readLocalApi();
      const resolved = resolveThreadTarget(target);

      if (confirmThreadDelete && localApi) {
        const title = resolved?.thread.title ?? "this thread";
        const confirmationResult = await settlePromise(() =>
          localApi.dialogs.confirm(
            [
              `Delete thread "${title}"?`,
              "This permanently clears conversation history for this thread.",
            ].join("\n"),
          ),
        );
        if (confirmationResult._tag === "Failure") {
          return confirmationResult;
        }
        if (!confirmationResult.value) {
          return AsyncResult.success(undefined);
        }
      }

      return deleteThread(target);
    },
    [confirmThreadDelete, deleteThread, resolveThreadTarget],
  );

  return useMemo(
    () => ({
      archiveThread,
      unarchiveThread,
      deleteThread,
      confirmAndDeleteThread,
      worktreeRemovalTarget,
      requestWorktreeRemoval,
      closeWorktreeRemovalDialog,
      completeWorktreeRemoval,
    }),
    [
      archiveThread,
      closeWorktreeRemovalDialog,
      completeWorktreeRemoval,
      confirmAndDeleteThread,
      deleteThread,
      requestWorktreeRemoval,
      unarchiveThread,
      worktreeRemovalTarget,
    ],
  );
}
