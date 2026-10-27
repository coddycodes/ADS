import { parseProjectKey, projectKey } from "@coddy/client-runtime/state/entities";
import type { ScopedProjectRef } from "@coddy/contracts";
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";

export const GIT_MANAGER_STORAGE_KEY = "coddy-studio:git-manager-state:v1";
const GIT_MANAGER_STORAGE_VERSION = 1;
const GIT_MANAGER_VIEW_STATE_LIMIT = 2;

export type GitManagerTab = "changes" | "history" | "tags";
export type GitManagerOpenDropdown = "branch" | "sync" | null;
export type GitManagerImageDiffMode = "two-up" | "swipe" | "onion" | "difference";

export interface SerializedGitManagerLineSelection {
  readonly type: "all" | "partial" | "none";
  readonly basis: "all" | "none";
  readonly diverging: ReadonlyArray<number>;
  readonly selectable: ReadonlyArray<number> | null;
  readonly area: "staged" | "unstaged";
  readonly generation: number;
}

export interface GitManagerToolbarViewState {
  readonly branchFilterText: string;
  readonly openDropdown: GitManagerOpenDropdown;
}

export interface GitManagerViewState {
  readonly selectedWorktreeCwd: string | null;
  readonly activeTab: GitManagerTab;
  readonly selectedRef: string | null;
  readonly selectedCommitSha: string | null;
  readonly multiCommitSelection: ReadonlyArray<string>;
  readonly selectedFilePath: string | null;
  readonly selectedStashSha: string | null;
  readonly stashPaneOpen: boolean;
  /** Tags tab sections the user collapsed: `local` or `remote:<name>`. */
  readonly collapsedTagSections: ReadonlyArray<string>;
  readonly imageDiffMode: GitManagerImageDiffMode;
  readonly providerPaneOpen: boolean;
  readonly lineSelectionByPath: Record<string, SerializedGitManagerLineSelection>;
  readonly filterText: string;
  readonly loadedPageCount: number;
  readonly loadedPageCursors: ReadonlyArray<number>;
  readonly scrollAnchor: string | null;
  readonly commitDraft: string;
  readonly lastUsedAt: number;
}

// The active tab is deliberately not persisted: the manager opens on History
// (or on Changes while a merge is pending) every time.
type PersistedGitManagerViewState = Omit<GitManagerViewState, "selectedWorktreeCwd" | "activeTab">;

interface PersistedGitManagerState {
  readonly byProjectKey: Record<string, PersistedGitManagerViewState>;
  readonly toolbarByProjectKey: Record<string, GitManagerToolbarViewState>;
}

export const DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE: GitManagerToolbarViewState = Object.freeze({
  branchFilterText: "",
  openDropdown: null,
});

export const DEFAULT_GIT_MANAGER_VIEW_STATE: GitManagerViewState = Object.freeze({
  selectedWorktreeCwd: null,
  activeTab: "history",
  selectedRef: null,
  selectedCommitSha: null,
  multiCommitSelection: Object.freeze([]),
  selectedFilePath: null,
  selectedStashSha: null,
  stashPaneOpen: false,
  collapsedTagSections: Object.freeze([]),
  imageDiffMode: "two-up",
  providerPaneOpen: false,
  lineSelectionByPath: Object.freeze({}),
  filterText: "",
  loadedPageCount: 0,
  loadedPageCursors: Object.freeze([]),
  scrollAnchor: null,
  commitDraft: "",
  lastUsedAt: 0,
});

interface GitManagerStoreState {
  readonly byProjectKey: Record<string, GitManagerViewState>;
  readonly toolbarByProjectKey: Record<string, GitManagerToolbarViewState>;
  readonly selectViewState: (ref: ScopedProjectRef) => GitManagerViewState;
  readonly selectToolbarViewState: (ref: ScopedProjectRef) => GitManagerToolbarViewState;
  readonly touchProject: (ref: ScopedProjectRef) => void;
  readonly setSelectedWorktree: (ref: ScopedProjectRef, cwd: string) => void;
  readonly setActiveTab: (ref: ScopedProjectRef, tab: GitManagerTab) => void;
  readonly setSelectedRef: (ref: ScopedProjectRef, name: string | null) => void;
  readonly setSelectedCommit: (ref: ScopedProjectRef, sha: string | null) => void;
  readonly setMultiCommitSelection: (ref: ScopedProjectRef, shas: ReadonlyArray<string>) => void;
  readonly setSelectedFile: (ref: ScopedProjectRef, path: string | null) => void;
  readonly setSelectedStash: (ref: ScopedProjectRef, sha: string | null) => void;
  readonly setStashPaneOpen: (ref: ScopedProjectRef, open: boolean) => void;
  readonly setTagSectionCollapsed: (
    ref: ScopedProjectRef,
    section: string,
    collapsed: boolean,
  ) => void;
  readonly setImageDiffMode: (ref: ScopedProjectRef, mode: GitManagerImageDiffMode) => void;
  readonly setProviderPaneOpen: (ref: ScopedProjectRef, open: boolean) => void;
  readonly setLineSelection: (
    ref: ScopedProjectRef,
    path: string,
    selection: SerializedGitManagerLineSelection | null,
  ) => void;
  readonly setFilterText: (ref: ScopedProjectRef, text: string) => void;
  readonly setLoadedPageCount: (ref: ScopedProjectRef, count: number) => void;
  readonly setLoadedPageCursors: (ref: ScopedProjectRef, cursors: ReadonlyArray<number>) => void;
  readonly setScrollAnchor: (ref: ScopedProjectRef, anchor: string | null) => void;
  readonly setCommitDraft: (ref: ScopedProjectRef, draft: string) => void;
  readonly setBranchFilterText: (ref: ScopedProjectRef, text: string) => void;
  readonly setOpenDropdown: (ref: ScopedProjectRef, value: GitManagerOpenDropdown) => void;
}

function nextLastUsedAt(byProjectKey: Record<string, GitManagerViewState>): number {
  let latest = 0;
  for (const viewState of Object.values(byProjectKey)) {
    latest = Math.max(latest, viewState.lastUsedAt);
  }
  return Math.max(Date.now(), latest + 1);
}

function retainMostRecent<T extends { readonly lastUsedAt: number }>(
  byProjectKey: Record<string, T>,
): Record<string, T> {
  const entries = Object.entries(byProjectKey);
  if (entries.length <= GIT_MANAGER_VIEW_STATE_LIMIT) return byProjectKey;

  entries.sort(
    ([leftKey, left], [rightKey, right]) =>
      right.lastUsedAt - left.lastUsedAt || leftKey.localeCompare(rightKey),
  );
  return Object.fromEntries(entries.slice(0, GIT_MANAGER_VIEW_STATE_LIMIT));
}

function updateProject(
  state: Pick<GitManagerStoreState, "byProjectKey" | "toolbarByProjectKey">,
  ref: ScopedProjectRef,
  update: (current: GitManagerViewState) => GitManagerViewState,
): Pick<GitManagerStoreState, "byProjectKey" | "toolbarByProjectKey"> {
  const key = projectKey(ref);
  const current = state.byProjectKey[key] ?? DEFAULT_GIT_MANAGER_VIEW_STATE;
  const updated = {
    ...update(current),
    lastUsedAt: nextLastUsedAt(state.byProjectKey),
  };
  const byProjectKey = retainMostRecent({ ...state.byProjectKey, [key]: updated });
  return {
    byProjectKey,
    toolbarByProjectKey: Object.fromEntries(
      Object.entries(state.toolbarByProjectKey).filter(([toolbarKey]) =>
        Object.hasOwn(byProjectKey, toolbarKey),
      ),
    ),
  };
}

function updateToolbarProject(
  state: Pick<GitManagerStoreState, "toolbarByProjectKey">,
  ref: ScopedProjectRef,
  update: (current: GitManagerToolbarViewState) => GitManagerToolbarViewState,
): Pick<GitManagerStoreState, "toolbarByProjectKey"> {
  const key = projectKey(ref);
  const current = state.toolbarByProjectKey[key] ?? DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE;
  return {
    toolbarByProjectKey: { ...state.toolbarByProjectKey, [key]: update(current) },
  };
}

function nullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function stringOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function nonNegativeIntegerOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : fallback;
}

function nonNegativeIntegerArray(value: unknown): ReadonlyArray<number> {
  if (!Array.isArray(value)) return [];
  return value.filter(
    (cursor): cursor is number =>
      typeof cursor === "number" && Number.isSafeInteger(cursor) && cursor >= 0,
  );
}

function nonEmptyUniqueStringArray(value: unknown): ReadonlyArray<string> {
  if (!Array.isArray(value)) return [];
  return [
    ...new Set(
      value.filter(
        (entry): entry is string => typeof entry === "string" && entry.trim().length > 0,
      ),
    ),
  ];
}

function sortedNonNegativeIntegerArray(value: unknown): ReadonlyArray<number> {
  return [...new Set(nonNegativeIntegerArray(value))].sort((left, right) => left - right);
}

function sanitizeLineSelection(value: unknown): SerializedGitManagerLineSelection | null {
  if (value === null || typeof value !== "object") return null;
  const candidate = value as Record<string, unknown>;
  const type =
    candidate.type === "all" || candidate.type === "partial" || candidate.type === "none"
      ? candidate.type
      : null;
  const basis = candidate.basis === "all" || candidate.basis === "none" ? candidate.basis : null;
  const area = candidate.area === "staged" || candidate.area === "unstaged" ? candidate.area : null;
  if (type === null || basis === null || area === null) return null;
  return {
    type,
    basis,
    diverging: sortedNonNegativeIntegerArray(candidate.diverging),
    selectable:
      candidate.selectable === null ? null : sortedNonNegativeIntegerArray(candidate.selectable),
    area,
    generation: nonNegativeIntegerOr(candidate.generation, 0),
  };
}

function sanitizeLineSelectionByPath(
  value: unknown,
): Record<string, SerializedGitManagerLineSelection> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  const result: Record<string, SerializedGitManagerLineSelection> = {};
  for (const [path, selection] of Object.entries(value)) {
    if (path.trim().length === 0) continue;
    const sanitized = sanitizeLineSelection(selection);
    if (sanitized !== null) result[path] = sanitized;
  }
  return result;
}

function sanitizeViewState(value: unknown): PersistedGitManagerViewState | null {
  if (value === null || typeof value !== "object") return null;
  const candidate = value as Record<string, unknown>;
  return {
    selectedRef: nullableString(candidate.selectedRef),
    selectedCommitSha: nullableString(candidate.selectedCommitSha),
    multiCommitSelection: nonEmptyUniqueStringArray(candidate.multiCommitSelection),
    selectedFilePath: nullableString(candidate.selectedFilePath),
    selectedStashSha: nullableString(candidate.selectedStashSha),
    stashPaneOpen: candidate.stashPaneOpen === true,
    collapsedTagSections: nonEmptyUniqueStringArray(candidate.collapsedTagSections),
    imageDiffMode:
      candidate.imageDiffMode === "swipe" ||
      candidate.imageDiffMode === "onion" ||
      candidate.imageDiffMode === "difference"
        ? candidate.imageDiffMode
        : "two-up",
    providerPaneOpen: candidate.providerPaneOpen === true,
    lineSelectionByPath: sanitizeLineSelectionByPath(candidate.lineSelectionByPath),
    filterText: stringOr(candidate.filterText, ""),
    loadedPageCount: nonNegativeIntegerOr(candidate.loadedPageCount, 0),
    loadedPageCursors: nonNegativeIntegerArray(candidate.loadedPageCursors),
    scrollAnchor: nullableString(candidate.scrollAnchor),
    commitDraft: stringOr(candidate.commitDraft, ""),
    lastUsedAt: nonNegativeIntegerOr(candidate.lastUsedAt, 0),
  };
}

function sanitizeToolbarViewState(value: unknown): GitManagerToolbarViewState | null {
  if (value === null || typeof value !== "object") return null;
  const candidate = value as Record<string, unknown>;
  return {
    branchFilterText: stringOr(candidate.branchFilterText, ""),
    openDropdown:
      candidate.openDropdown === "branch" || candidate.openDropdown === "sync"
        ? candidate.openDropdown
        : null,
  };
}

export function sanitizePersistedGitManagerState(
  persistedState: unknown,
): PersistedGitManagerState {
  if (persistedState === null || typeof persistedState !== "object") {
    return { byProjectKey: {}, toolbarByProjectKey: {} };
  }
  const rawState = persistedState as {
    byProjectKey?: unknown;
    toolbarByProjectKey?: unknown;
  };
  const raw = rawState.byProjectKey;
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return { byProjectKey: {}, toolbarByProjectKey: {} };
  }

  const byProjectKey: Record<string, PersistedGitManagerViewState> = {};
  for (const [key, value] of Object.entries(raw)) {
    try {
      parseProjectKey(key);
    } catch {
      continue;
    }
    const viewState = sanitizeViewState(value);
    if (viewState !== null) byProjectKey[key] = viewState;
  }
  const retainedByProjectKey = retainMostRecent(byProjectKey);
  const toolbarByProjectKey: Record<string, GitManagerToolbarViewState> = {};
  const rawToolbar = rawState.toolbarByProjectKey;
  if (rawToolbar !== null && typeof rawToolbar === "object" && !Array.isArray(rawToolbar)) {
    for (const [key, value] of Object.entries(rawToolbar)) {
      if (!Object.hasOwn(retainedByProjectKey, key)) continue;
      const toolbarViewState = sanitizeToolbarViewState(value);
      if (toolbarViewState !== null) toolbarByProjectKey[key] = toolbarViewState;
    }
  }
  return { byProjectKey: retainedByProjectKey, toolbarByProjectKey };
}

function restoreGitManagerState(
  persistedState: unknown,
): Pick<GitManagerStoreState, "byProjectKey" | "toolbarByProjectKey"> {
  const persisted = sanitizePersistedGitManagerState(persistedState);
  return {
    byProjectKey: Object.fromEntries(
      Object.entries(persisted.byProjectKey).map(([key, viewState]) => [
        key,
        { ...viewState, selectedWorktreeCwd: null, activeTab: "history" },
      ]),
    ),
    toolbarByProjectKey: persisted.toolbarByProjectKey,
  };
}

export const useGitManagerStore = create<GitManagerStoreState>()(
  persist(
    (set, get) => ({
      byProjectKey: {},
      toolbarByProjectKey: {},
      selectViewState: (ref) =>
        get().byProjectKey[projectKey(ref)] ?? DEFAULT_GIT_MANAGER_VIEW_STATE,
      selectToolbarViewState: (ref) =>
        get().toolbarByProjectKey[projectKey(ref)] ?? DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE,
      touchProject: (ref) => set((state) => updateProject(state, ref, (current) => current)),
      setSelectedWorktree: (ref, cwd) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, selectedWorktreeCwd: cwd })),
        ),
      setActiveTab: (ref, tab) =>
        set((state) => updateProject(state, ref, (current) => ({ ...current, activeTab: tab }))),
      setSelectedRef: (ref, name) =>
        set((state) => updateProject(state, ref, (current) => ({ ...current, selectedRef: name }))),
      setSelectedCommit: (ref, sha) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, selectedCommitSha: sha })),
        ),
      setMultiCommitSelection: (ref, shas) =>
        set((state) =>
          updateProject(state, ref, (current) => ({
            ...current,
            multiCommitSelection: nonEmptyUniqueStringArray(shas),
          })),
        ),
      setSelectedFile: (ref, path) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, selectedFilePath: path })),
        ),
      setSelectedStash: (ref, sha) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, selectedStashSha: sha })),
        ),
      setStashPaneOpen: (ref, open) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, stashPaneOpen: open })),
        ),
      setTagSectionCollapsed: (ref, section, collapsed) =>
        set((state) =>
          updateProject(state, ref, (current) => {
            const without = current.collapsedTagSections.filter((entry) => entry !== section);
            return {
              ...current,
              collapsedTagSections: collapsed ? [...without, section] : without,
            };
          }),
        ),
      setImageDiffMode: (ref, mode) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, imageDiffMode: mode })),
        ),
      setProviderPaneOpen: (ref, open) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, providerPaneOpen: open })),
        ),
      setLineSelection: (ref, path, selection) =>
        set((state) =>
          updateProject(state, ref, (current) => {
            const lineSelectionByPath = { ...current.lineSelectionByPath };
            if (selection === null) {
              delete lineSelectionByPath[path];
            } else {
              const sanitized = sanitizeLineSelection(selection);
              if (sanitized !== null) lineSelectionByPath[path] = sanitized;
            }
            return { ...current, lineSelectionByPath };
          }),
        ),
      setFilterText: (ref, text) =>
        set((state) => updateProject(state, ref, (current) => ({ ...current, filterText: text }))),
      setLoadedPageCount: (ref, count) =>
        set((state) =>
          updateProject(state, ref, (current) => ({
            ...current,
            loadedPageCount: nonNegativeIntegerOr(count, current.loadedPageCount),
          })),
        ),
      setLoadedPageCursors: (ref, cursors) =>
        set((state) =>
          updateProject(state, ref, (current) => ({
            ...current,
            loadedPageCursors: nonNegativeIntegerArray(cursors),
          })),
        ),
      setScrollAnchor: (ref, anchor) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, scrollAnchor: anchor })),
        ),
      setCommitDraft: (ref, draft) =>
        set((state) =>
          updateProject(state, ref, (current) => ({ ...current, commitDraft: draft })),
        ),
      setBranchFilterText: (ref, text) =>
        set((state) =>
          updateToolbarProject(state, ref, (current) => ({ ...current, branchFilterText: text })),
        ),
      setOpenDropdown: (ref, value) =>
        set((state) =>
          updateToolbarProject(state, ref, (current) => ({ ...current, openDropdown: value })),
        ),
    }),
    {
      name: GIT_MANAGER_STORAGE_KEY,
      version: GIT_MANAGER_STORAGE_VERSION,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => sanitizePersistedGitManagerState(state),
      migrate: sanitizePersistedGitManagerState,
      merge: (persistedState, currentState) => ({
        ...currentState,
        ...restoreGitManagerState(persistedState),
      }),
    },
  ),
);
