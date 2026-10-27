import { useAtomValue } from "@effect/atom-react";
import {
  type ArchivedSnapshotEntry,
  createArchivedThreadSnapshotsAtomFamily,
  makeArchivedThreadsEnvironmentKey,
  parseArchivedThreadsEnvironmentKey,
} from "@coddy/client-runtime/state/threads";
import type { EnvironmentId } from "@coddy/contracts";
import { useCallback, useEffect, useMemo } from "react";

import { orchestrationEnvironment } from "../state/orchestration";
import { appAtomRegistry } from "../rpc/atomRegistry";

function archivedSnapshotAtom(environmentId: EnvironmentId) {
  return orchestrationEnvironment.archivedShellSnapshot({
    environmentId,
    input: {},
  });
}

const archivedSnapshotsAtom = createArchivedThreadSnapshotsAtomFamily({
  getSnapshotAtom: archivedSnapshotAtom,
  labelPrefix: "web:archived-thread-snapshots",
});

export function refreshArchivedThreadsForEnvironment(environmentId: EnvironmentId): void {
  appAtomRegistry.refresh(archivedSnapshotAtom(environmentId));
}

export function useArchivedThreadSnapshots(
  environmentIds: ReadonlyArray<EnvironmentId>,
  options?: {
    /**
     * Refetch once when the consumer mounts (default). The lifecycle reconciler keeps
     * the snapshot loaded for the app's lifetime, so a list that opens later would
     * otherwise show a snapshot that misses changes made elsewhere to archived threads.
     */
    readonly revalidateOnMount?: boolean;
  },
): {
  readonly snapshots: ReadonlyArray<ArchivedSnapshotEntry>;
  readonly error: string | null;
  readonly isLoading: boolean;
  readonly refresh: () => void;
} {
  const environmentKey = useMemo(
    () => makeArchivedThreadsEnvironmentKey(environmentIds),
    [environmentIds],
  );
  const result = useAtomValue(archivedSnapshotsAtom(environmentKey));
  const refresh = useCallback(() => {
    for (const environmentId of environmentIds) {
      appAtomRegistry.refresh(archivedSnapshotAtom(environmentId));
    }
  }, [environmentIds]);
  const revalidateOnMount = options?.revalidateOnMount ?? true;
  useEffect(() => {
    if (!revalidateOnMount) return;
    for (const environmentId of parseArchivedThreadsEnvironmentKey(environmentKey)) {
      const atom = archivedSnapshotAtom(environmentId);
      // A request already in flight answers this mount; never start a second one.
      if (!appAtomRegistry.get(atom).waiting) {
        appAtomRegistry.refresh(atom);
      }
    }
  }, [environmentKey, revalidateOnMount]);

  return {
    ...result,
    refresh,
  };
}
