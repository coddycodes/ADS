import { scopedThreadKey } from "@coddy/client-runtime/environment";
import { useRouter } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";

import { useSidebarWorkspaceMetaStore } from "../../sidebarWorkspaceMetaStore";
import { resolveThreadRouteRef } from "../../threadRoutes";
import type { AgentRow } from "./agentsSection.logic";

export function detectUnreadTransitions(input: {
  readonly previous: ReadonlyMap<string, string>;
  readonly rows: ReadonlyArray<AgentRow>;
  readonly openThreadKey: string | null;
}): {
  readonly next: ReadonlyMap<string, string>;
  readonly markUnreadKeys: ReadonlyArray<string>;
} {
  const next = new Map<string, string>();
  const markUnreadKeys: string[] = [];

  for (const row of input.rows) {
    const latestTurn = row.shell.latestTurn;
    if (latestTurn === null) {
      continue;
    }

    const signature = `${latestTurn.turnId}:${latestTurn.state}`;
    next.set(row.key, signature);

    const settled =
      latestTurn.state === "completed" ||
      latestTurn.state === "interrupted" ||
      latestTurn.state === "error";
    if (
      input.previous.has(row.key) &&
      input.previous.get(row.key) !== signature &&
      settled &&
      row.key !== input.openThreadKey
    ) {
      markUnreadKeys.push(row.key);
    }
  }

  return { next, markUnreadKeys };
}

export function useAgentsUnread(rows: ReadonlyArray<AgentRow>): void {
  const previous = useRef<ReadonlyMap<string, string>>(new Map());
  const router = useRouter({ warn: false }) as ReturnType<typeof useRouter> | null;
  const subscribeToRoute = useCallback(
    (onStoreChange: () => void) =>
      typeof router?.subscribe === "function"
        ? router.subscribe("onResolved", onStoreChange)
        : () => undefined,
    [router],
  );
  const getOpenThreadKey = useCallback(() => {
    const matches = router?.state?.matches;
    const params = matches?.[matches.length - 1]?.params ?? {};
    const routeThreadRef = resolveThreadRouteRef(params);
    return routeThreadRef === null ? null : scopedThreadKey(routeThreadRef);
  }, [router]);
  const openThreadKey = useSyncExternalStore(subscribeToRoute, getOpenThreadKey, getOpenThreadKey);
  const markUnread = useSidebarWorkspaceMetaStore((state) => state.markUnread);

  useEffect(() => {
    const result = detectUnreadTransitions({
      previous: previous.current,
      rows,
      openThreadKey,
    });
    previous.current = result.next;
    for (const key of result.markUnreadKeys) {
      markUnread(key);
    }
  }, [markUnread, openThreadKey, rows]);
}
