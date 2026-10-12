import { useAtomSubscribe, useAtomValue } from "@effect/atom-react";
import {
  parseScopedThreadKey,
  scopedThreadKey,
  scopeThreadRef,
} from "@bibcode/client-runtime/environment";
import type { EnvironmentId, TerminalMetadataStreamEvent, ThreadId } from "@bibcode/contracts";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  applyPanelThreadAdoption,
  applyTerminalMetadataAdoption,
  createCenterPanelAdoptionTracker,
} from "./centerPanelAdoption";

import { useCenterPanelStore } from "./centerPanelStore";
import { useComposerDraftStore } from "./composerDraftStore";
import { useArchivedThreadSnapshots } from "./lib/archivedThreadsState";
import { useRightPanelStore } from "./rightPanelStore";
import { useEnvironments } from "./state/environments";
import { environmentShell } from "./state/shell";
import { terminalEnvironment } from "./state/terminal";

function collectPersistedThreadIds(environmentId: EnvironmentId): Set<ThreadId> {
  const threadIds = new Set<ThreadId>();
  for (const [threadKey, state] of Object.entries(useCenterPanelStore.getState().byThreadKey)) {
    const hostRef = parseScopedThreadKey(threadKey);
    if (!hostRef || hostRef.environmentId !== environmentId) continue;
    threadIds.add(hostRef.threadId);
    for (const surface of state.surfaces) {
      if (surface.kind === "chat") {
        threadIds.add(surface.threadId);
      }
    }
  }
  for (const threadKey of Object.keys(useRightPanelStore.getState().byThreadKey)) {
    const ref = parseScopedThreadKey(threadKey);
    if (ref?.environmentId === environmentId) {
      threadIds.add(ref.threadId);
    }
  }
  return threadIds;
}

export function reconcileThreadPanelState(
  environmentId: EnvironmentId,
  retainedThreadIds: ReadonlySet<ThreadId>,
): void {
  const centerPanelState = useCenterPanelStore.getState();
  for (const threadId of collectPersistedThreadIds(environmentId)) {
    const threadRef = scopeThreadRef(environmentId, threadId);
    if (retainedThreadIds.has(threadId)) {
      centerPanelState.releaseChatPanelReservation(threadRef);
      continue;
    }
    if (centerPanelState.pendingChatPanelThreadKeys.has(scopedThreadKey(threadRef))) {
      continue;
    }
    centerPanelState.removeThread(threadRef);
    useRightPanelStore.getState().removeThread(threadRef);
  }
}

function liveThreadMembershipKey(threads: ReadonlyArray<{ readonly id: ThreadId }>): string {
  return threads
    .map((thread) => thread.id)
    .toSorted()
    .join("\u001f");
}

function EnvironmentThreadLifecycleReconciler({
  environmentId,
}: {
  readonly environmentId: EnvironmentId;
}) {
  const shellState = useAtomValue(environmentShell.stateValueAtom(environmentId));
  const archivedEnvironmentIds = useMemo(() => [environmentId], [environmentId]);
  const {
    error: archivedError,
    isLoading: archivedIsLoading,
    refresh: refreshArchived,
    snapshots: archivedSnapshots,
  } = useArchivedThreadSnapshots(archivedEnvironmentIds);
  const draftThreadsByThreadKey = useComposerDraftStore((state) => state.draftThreadsByThreadKey);
  const draftThreadIds = useMemo(
    () =>
      new Set<ThreadId>(
        Object.values(draftThreadsByThreadKey).flatMap((draft) =>
          draft.environmentId === environmentId ? [draft.threadId] : [],
        ),
      ),
    [draftThreadsByThreadKey, environmentId],
  );
  // The live thread ids and the live sequence at which they last changed.
  const liveMembershipRef = useRef<{ readonly key: string; readonly sequence: number } | null>(
    null,
  );
  const lastArchivedRefreshSequenceRef = useRef<number | null>(null);
  const [adoptionTracker] = useState(createCenterPanelAdoptionTracker);
  const terminalMetadataEventsAtom = useMemo(
    () => terminalEnvironment.metadataEvents({ environmentId, input: {} }),
    [environmentId],
  );
  const handleTerminalMetadataEvent = useCallback(
    (result: AsyncResult.AsyncResult<ReadonlyArray<TerminalMetadataStreamEvent>, unknown>) => {
      if (!AsyncResult.isSuccess(result)) return;
      for (const event of result.value) {
        applyTerminalMetadataAdoption(environmentId, adoptionTracker, event);
      }
    },
    [adoptionTracker, environmentId],
  );
  useAtomSubscribe(terminalMetadataEventsAtom, handleTerminalMetadataEvent, { immediate: true });

  useEffect(() => {
    if (shellState.status !== "live" || Option.isNone(shellState.snapshot)) return;
    applyPanelThreadAdoption(environmentId, adoptionTracker, shellState.snapshot.value.threads);
  }, [adoptionTracker, environmentId, shellState]);

  // The archived snapshot is refetched only when a thread enters or leaves the live list
  // (create, archive, unarchive, delete, panel close), never on every live sequence
  // advance of a running turn. A change elsewhere to an already archived thread waits for
  // the next such change; meanwhile its state is retained, never dropped.
  useEffect(() => {
    if (shellState.status !== "live" || Option.isNone(shellState.snapshot)) {
      liveMembershipRef.current = null;
      lastArchivedRefreshSequenceRef.current = null;
      return;
    }
    const liveSnapshot = shellState.snapshot.value;
    const membershipKey = liveThreadMembershipKey(liveSnapshot.threads);
    if (liveMembershipRef.current?.key !== membershipKey) {
      liveMembershipRef.current = { key: membershipKey, sequence: liveSnapshot.snapshotSequence };
    }
    // The archived atom runs one request at a time; never start another while it runs.
    if (archivedIsLoading) return;

    const lifecycleSequence = liveMembershipRef.current.sequence;
    const archivedSnapshot = archivedSnapshots[0]?.snapshot;
    // Taken at or after the last membership change, the archived snapshot knows every
    // thread that left the live list. A newer one may miss a thread unarchived after the
    // live snapshot, so wait for the live shell to catch up instead.
    const snapshotsAreAuthoritative =
      archivedError === null &&
      archivedSnapshot !== undefined &&
      archivedSnapshot.snapshotSequence >= lifecycleSequence &&
      archivedSnapshot.snapshotSequence <= liveSnapshot.snapshotSequence;
    if (snapshotsAreAuthoritative) {
      lastArchivedRefreshSequenceRef.current = null;
      const retainedThreadIds = new Set<ThreadId>([
        ...liveSnapshot.threads.map((thread) => thread.id),
        // A closed chat panel is archived and keeps no tab; reopening reserves a new one.
        ...archivedSnapshot.threads.flatMap((thread) =>
          thread.kind === "panel" ? [] : [thread.id],
        ),
        ...draftThreadIds,
      ]);
      reconcileThreadPanelState(environmentId, retainedThreadIds);
      return;
    }

    if (
      archivedSnapshot !== undefined &&
      archivedSnapshot.snapshotSequence > liveSnapshot.snapshotSequence
    ) {
      return;
    }
    // One request per membership change, even when it fails.
    if (lastArchivedRefreshSequenceRef.current === lifecycleSequence) {
      return;
    }
    lastArchivedRefreshSequenceRef.current = lifecycleSequence;
    refreshArchived();
  }, [
    archivedError,
    archivedIsLoading,
    archivedSnapshots,
    draftThreadIds,
    environmentId,
    refreshArchived,
    shellState,
  ]);

  return null;
}

/** Reconciles renderer-persisted thread UI state from authoritative server lifecycle knowledge. */
export function ThreadLifecycleReconciler() {
  const { environments } = useEnvironments();
  return environments.map((environment) => (
    <EnvironmentThreadLifecycleReconciler
      key={environment.environmentId}
      environmentId={environment.environmentId}
    />
  ));
}
