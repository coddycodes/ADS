import {
  type CheckpointDiffTarget,
  type ComposerPathSearchTarget,
} from "@coddy/client-runtime/state/threads";
import type { EnvironmentId, OrchestrationThread, ThreadId } from "@coddy/contracts";
import * as Option from "effect/Option";
import { useEffect, useMemo, useState } from "react";

import { orchestrationEnvironment } from "./orchestration";
import { projectEnvironment } from "./projects";
import { useEnvironmentQuery } from "./query";
import { useEnvironmentThread } from "./threads";

const COMPOSER_PATH_SEARCH_DEBOUNCE_MS = 120;
const COMPOSER_PATH_SEARCH_LIMIT = 80;

export interface ThreadDetailView {
  readonly data: OrchestrationThread | null;
  readonly error: string | null;
  readonly isPending: boolean;
  readonly isDeleted: boolean;
}

function useDebouncedValue<A>(value: A, delayMs: number): A {
  const [debounced, setDebounced] = useState(value);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setDebounced(value);
    }, delayMs);
    return () => {
      window.clearTimeout(timer);
    };
  }, [delayMs, value]);

  return debounced;
}

export function useThreadDetail(
  environmentId: EnvironmentId | null,
  threadId: ThreadId | null,
): ThreadDetailView {
  const state = useEnvironmentThread(environmentId, threadId);
  return {
    data: Option.getOrNull(state.data),
    error: Option.getOrNull(state.error),
    isPending: state.status === "synchronizing",
    isDeleted: state.status === "deleted",
  };
}

export function useComposerPathSearch(target: ComposerPathSearchTarget) {
  const normalizedTarget = useMemo(
    () => ({
      environmentId: target.environmentId,
      cwd: target.cwd,
      query: target.query?.trim() ?? "",
    }),
    [target.cwd, target.environmentId, target.query],
  );
  const debouncedTarget = useDebouncedValue(normalizedTarget, COMPOSER_PATH_SEARCH_DEBOUNCE_MS);
  const result = useEnvironmentQuery(
    debouncedTarget.environmentId !== null && debouncedTarget.cwd !== null
      ? debouncedTarget.query.length > 0
        ? projectEnvironment.searchEntries({
            environmentId: debouncedTarget.environmentId,
            input: {
              cwd: debouncedTarget.cwd,
              query: debouncedTarget.query,
              limit: COMPOSER_PATH_SEARCH_LIMIT,
            },
          })
        : projectEnvironment.listEntries({
            environmentId: debouncedTarget.environmentId,
            input: {
              cwd: debouncedTarget.cwd,
              limit: COMPOSER_PATH_SEARCH_LIMIT,
            },
          })
      : null,
  );

  const isPending = normalizedTarget.query !== debouncedTarget.query || result.isPending;
  // A refreshed search for the same workspace target keeps the last loaded entries
  // visible until its result arrives, so the menu never unmounts its rows between
  // keystrokes. Entries never carry over to another environment or cwd.
  const [retained, setRetained] = useState<{
    readonly key: string;
    readonly source: NonNullable<typeof result.data>;
    readonly entries: NonNullable<typeof result.data>["entries"];
  } | null>(null);
  const targetKey =
    normalizedTarget.environmentId === null || normalizedTarget.cwd === null
      ? null
      : `${normalizedTarget.environmentId}\0${normalizedTarget.cwd}`;
  const loadedEntries = result.data?.entries.slice(0, COMPOSER_PATH_SEARCH_LIMIT) ?? null;
  if (targetKey === null) {
    if (retained !== null) setRetained(null);
  } else if (
    result.data !== null &&
    loadedEntries !== null &&
    (retained?.key !== targetKey || retained.source !== result.data)
  ) {
    setRetained({ key: targetKey, source: result.data, entries: loadedEntries });
  }
  const entries =
    loadedEntries ??
    (isPending && targetKey !== null && retained !== null && retained.key === targetKey
      ? retained.entries
      : []);

  return {
    entries,
    error: result.error,
    isPending,
    refresh: result.refresh,
  };
}

export function useCheckpointDiff(
  target: CheckpointDiffTarget,
  options?: { readonly enabled?: boolean },
) {
  const enabled =
    options?.enabled !== false &&
    target.environmentId !== null &&
    target.threadId !== null &&
    target.fromTurnCount !== null &&
    target.toTurnCount !== null;
  const fullThreadTarget =
    enabled && target.fromTurnCount === 0
      ? {
          environmentId: target.environmentId!,
          input: {
            threadId: target.threadId!,
            toTurnCount: target.toTurnCount!,
            ignoreWhitespace: target.ignoreWhitespace,
          },
        }
      : null;
  const turnTarget =
    enabled && target.fromTurnCount !== 0
      ? {
          environmentId: target.environmentId!,
          input: {
            threadId: target.threadId!,
            fromTurnCount: target.fromTurnCount!,
            toTurnCount: target.toTurnCount!,
            ignoreWhitespace: target.ignoreWhitespace,
          },
        }
      : null;
  const fullThread = useEnvironmentQuery(
    fullThreadTarget === null ? null : orchestrationEnvironment.fullThreadDiff(fullThreadTarget),
  );
  const turn = useEnvironmentQuery(
    turnTarget === null ? null : orchestrationEnvironment.turnDiff(turnTarget),
  );
  return fullThreadTarget === null ? turn : fullThread;
}
