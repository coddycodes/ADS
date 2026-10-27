/**
 * Sidebar workspace-row metadata: pin state and unread markers.
 *
 * Orca-parity workspace rows (primary + worktree threads) carry small
 * app-level metadata that is not part of the server's thread model — pinning
 * for manual ordering and an unread marker cleared on visit. Keyed by
 * `scopedThreadKey` (environmentId+threadId), mirroring the pattern used by
 * `rightPanelStore.ts`.
 */
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";

const SIDEBAR_WORKSPACE_META_STORAGE_KEY = "coddy-studio:sidebar-workspace-meta:v1";
const SIDEBAR_WORKSPACE_META_STORAGE_VERSION = 1;

export interface SidebarWorkspaceMetaState {
  pinnedThreadKeys: string[];
  unreadThreadKeys: string[];
  togglePinned: (key: string) => void;
  markUnread: (key: string) => void;
  markRead: (key: string) => void;
}

function withToggled(keys: readonly string[], key: string): string[] {
  return keys.includes(key) ? keys.filter((existing) => existing !== key) : [...keys, key];
}

function withAdded(keys: readonly string[], key: string): string[] {
  return keys.includes(key) ? [...keys] : [...keys, key];
}

function withRemoved(keys: readonly string[], key: string): string[] {
  return keys.includes(key) ? keys.filter((existing) => existing !== key) : [...keys];
}

export function migratePersistedSidebarWorkspaceMetaState(persistedState: unknown): {
  pinnedThreadKeys: string[];
  unreadThreadKeys: string[];
} {
  if (!persistedState || typeof persistedState !== "object") {
    return { pinnedThreadKeys: [], unreadThreadKeys: [] };
  }
  const state = persistedState as {
    pinnedThreadKeys?: unknown;
    unreadThreadKeys?: unknown;
  };
  const pinnedThreadKeys = Array.isArray(state.pinnedThreadKeys)
    ? state.pinnedThreadKeys.filter((key): key is string => typeof key === "string")
    : [];
  const unreadThreadKeys = Array.isArray(state.unreadThreadKeys)
    ? state.unreadThreadKeys.filter((key): key is string => typeof key === "string")
    : [];
  return { pinnedThreadKeys, unreadThreadKeys };
}

export const useSidebarWorkspaceMetaStore = create<SidebarWorkspaceMetaState>()(
  persist(
    (set) => ({
      pinnedThreadKeys: [],
      unreadThreadKeys: [],
      togglePinned: (key) =>
        set((state) => ({ pinnedThreadKeys: withToggled(state.pinnedThreadKeys, key) })),
      markUnread: (key) =>
        set((state) => ({ unreadThreadKeys: withAdded(state.unreadThreadKeys, key) })),
      markRead: (key) =>
        set((state) => ({ unreadThreadKeys: withRemoved(state.unreadThreadKeys, key) })),
    }),
    {
      name: SIDEBAR_WORKSPACE_META_STORAGE_KEY,
      version: SIDEBAR_WORKSPACE_META_STORAGE_VERSION,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => ({
        pinnedThreadKeys: state.pinnedThreadKeys,
        unreadThreadKeys: state.unreadThreadKeys,
      }),
      migrate: migratePersistedSidebarWorkspaceMetaState,
    },
  ),
);

export function selectIsPinned(pinnedThreadKeys: readonly string[], key: string): boolean {
  return pinnedThreadKeys.includes(key);
}

export function selectIsUnread(unreadThreadKeys: readonly string[], key: string): boolean {
  return unreadThreadKeys.includes(key);
}
