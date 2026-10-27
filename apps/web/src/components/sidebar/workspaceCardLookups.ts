import { scopeThreadRef, scopedThreadKey } from "@coddy/client-runtime/environment";
import {
  ThreadId,
  type DiscoveredLocalServer,
  type EnvironmentId,
  type TerminalSummary,
  type VcsAdoptedWorktreeStatus,
} from "@coddy/contracts";
import type { SidebarThreadSummary } from "../../types";

export type CardKeyThread = Pick<SidebarThreadSummary, "id" | "environmentId">;
export interface WorkspaceCardSources {
  readonly adopted: readonly {
    environmentId: EnvironmentId;
    rows: readonly VcsAdoptedWorktreeStatus[];
  }[];
  readonly terminals: readonly {
    environmentId: EnvironmentId;
    rows: readonly TerminalSummary[];
  }[];
  readonly ports: readonly {
    environmentId: EnvironmentId;
    rows: readonly DiscoveredLocalServer[];
  }[];
}
export const EMPTY_CARD_SOURCES: WorkspaceCardSources = {
  adopted: [],
  terminals: [],
  ports: [],
};
export const EMPTY_CARD_TERMINALS: readonly string[] = [];
export const EMPTY_CARD_PORTS: readonly DiscoveredLocalServer[] = [];

const keyOf = (environmentId: EnvironmentId, threadId: string) =>
  scopedThreadKey(scopeThreadRef(environmentId, ThreadId.make(threadId)));

export function buildWorkspaceCardKeys(
  threads: readonly CardKeyThread[],
  pinnedKeys: readonly string[],
  unreadKeys: readonly string[],
) {
  const pinned = new Set(pinnedKeys);
  const unread = new Set(unreadKeys);
  return new Map(
    threads.map((thread) => {
      const key = keyOf(thread.environmentId, thread.id);
      return [thread, { key, pinned: pinned.has(key), unread: unread.has(key) }] as const;
    }),
  );
}

export function buildAdoptedWorkspaceMap(sources: WorkspaceCardSources["adopted"]) {
  const result = new Map<string, VcsAdoptedWorktreeStatus>();
  for (const { environmentId, rows } of sources) {
    for (const row of rows) result.set(keyOf(environmentId, row.threadId), row);
  }
  return result;
}

export function buildRunningTerminalMap(sources: WorkspaceCardSources["terminals"]) {
  const result = new Map<string, string[]>();
  for (const { environmentId, rows } of sources) {
    for (const row of rows) {
      if (!row.hasRunningSubprocess) continue;
      const key = keyOf(environmentId, row.threadId);
      const values = result.get(key);
      if (values) values.push(row.terminalId);
      else result.set(key, [row.terminalId]);
    }
  }
  return result;
}

export function buildDiscoveredPortMap(sources: WorkspaceCardSources["ports"]) {
  const result = new Map<string, DiscoveredLocalServer[]>();
  for (const { environmentId, rows } of sources) {
    for (const row of rows) {
      if (!row.terminal) continue;
      const key = keyOf(environmentId, row.terminal.threadId);
      const values = result.get(key);
      if (values) values.push(row);
      else result.set(key, [row]);
    }
  }
  return result;
}
