import { scopedThreadKey } from "@coddy/client-runtime/environment";
import type { ScopedThreadRef } from "@coddy/contracts";
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";

export interface SourceControlDraft {
  message: string;
}

const DEFAULT_DRAFT: SourceControlDraft = { message: "" };

interface SourceControlPanelStoreState {
  byThreadKey: Record<string, SourceControlDraft>;
  byCwdKey: Record<string, SourceControlDraft>;
  /**
   * Changes view preference: pending files grouped under folder headers
   * (default) or one flat list. Panel-wide, not per section or per thread.
   */
  sourceControlGroupByFolder: boolean;
  setSourceControlGroupByFolder: (groupByFolder: boolean) => void;
  setMessage: (ref: ScopedThreadRef, message: string) => void;
  clearDraft: (ref: ScopedThreadRef) => void;
  removeThread: (ref: ScopedThreadRef) => void;
  setCwdMessage: (cwdKey: string, message: string) => void;
  clearCwdDraft: (cwdKey: string) => void;
  promoteThreadDraft: (threadKey: string, cwdKey: string) => void;
}

function updateDraft(
  state: SourceControlPanelStoreState,
  ref: ScopedThreadRef,
  updater: (draft: SourceControlDraft) => SourceControlDraft,
): { byThreadKey: Record<string, SourceControlDraft> } {
  const key = scopedThreadKey(ref);
  const previous = state.byThreadKey[key] ?? DEFAULT_DRAFT;
  return { byThreadKey: { ...state.byThreadKey, [key]: updater(previous) } };
}

export const useSourceControlPanelStore = create<SourceControlPanelStoreState>()(
  persist(
    (set) => ({
      byThreadKey: {},
      byCwdKey: {},
      sourceControlGroupByFolder: true,
      setSourceControlGroupByFolder: (groupByFolder) =>
        set(() => ({ sourceControlGroupByFolder: groupByFolder })),
      setMessage: (ref, message) =>
        set((state) => updateDraft(state, ref, (draft) => ({ ...draft, message }))),
      clearDraft: (ref) => set((state) => updateDraft(state, ref, () => ({ ...DEFAULT_DRAFT }))),
      removeThread: (ref) =>
        set((state) => {
          const key = scopedThreadKey(ref);
          if (!(key in state.byThreadKey)) return state;
          const { [key]: _removed, ...byThreadKey } = state.byThreadKey;
          return { byThreadKey };
        }),
      setCwdMessage: (cwdKey, message) =>
        set((state) => ({
          byCwdKey: {
            ...state.byCwdKey,
            [cwdKey]: { message },
          },
        })),
      clearCwdDraft: (cwdKey) =>
        set((state) => {
          if (!(cwdKey in state.byCwdKey)) return state;
          const { [cwdKey]: _removed, ...byCwdKey } = state.byCwdKey;
          return { byCwdKey };
        }),
      promoteThreadDraft: (threadKey, cwdKey) =>
        set((state) => {
          const legacyDraft = state.byThreadKey[threadKey];
          if (legacyDraft === undefined) return state;
          const { [threadKey]: _promoted, ...byThreadKey } = state.byThreadKey;
          if (cwdKey in state.byCwdKey || legacyDraft.message.length === 0) {
            return { byThreadKey };
          }
          return {
            byThreadKey,
            byCwdKey: {
              ...state.byCwdKey,
              [cwdKey]: { ...legacyDraft },
            },
          };
        }),
    }),
    {
      name: "coddy-studio:source-control-panel-state:v1",
      version: 2,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      migrate: (persistedState) => {
        const state = persistedState as Partial<SourceControlPanelStoreState> | undefined;
        // `sourceControlGroupByFolder` is intentionally absent: persist's shallow
        // merge keeps a stored value and falls back to the grouped default.
        return {
          byThreadKey: state?.byThreadKey ?? {},
          byCwdKey: state?.byCwdKey ?? {},
        };
      },
      partialize: (state) => ({
        byThreadKey: state.byThreadKey,
        byCwdKey: state.byCwdKey,
        sourceControlGroupByFolder: state.sourceControlGroupByFolder,
      }),
    },
  ),
);

export function selectThreadSourceControlDraft(
  byThreadKey: Record<string, SourceControlDraft>,
  ref: ScopedThreadRef | null | undefined,
): SourceControlDraft {
  if (!ref) return DEFAULT_DRAFT;
  return byThreadKey[scopedThreadKey(ref)] ?? DEFAULT_DRAFT;
}

export function selectCwdSourceControlDraft(
  byCwdKey: Record<string, SourceControlDraft>,
  cwdKey: string,
): SourceControlDraft {
  return byCwdKey[cwdKey] ?? DEFAULT_DRAFT;
}
