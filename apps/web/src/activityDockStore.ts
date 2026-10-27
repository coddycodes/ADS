import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";

export const ACTIVITY_DOCK_STORAGE_KEY = "coddy-studio:activity-dock-state:v1";
const ACTIVITY_DOCK_STORAGE_VERSION = 1;
export const ACTIVITY_DOCK_MAX_PROJECTS = 256;
const ACTIVITY_DOCK_PROJECT_KEY_PART_MAX_LENGTH = 256;
export const ACTIVITY_DOCK_PROJECT_KEY_MAX_LENGTH =
  ACTIVITY_DOCK_PROJECT_KEY_PART_MAX_LENGTH * 2 + 1;

export interface ActivityDockStoreState {
  expandedByProject: Record<string, boolean>;
  setExpanded: (projectKey: string, expanded: boolean) => void;
  toggleExpanded: (projectKey: string) => void;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isCanonicalProjectKey(projectKey: unknown): projectKey is string {
  if (
    typeof projectKey !== "string" ||
    projectKey.length === 0 ||
    projectKey.length > ACTIVITY_DOCK_PROJECT_KEY_MAX_LENGTH ||
    projectKey.trim() !== projectKey
  ) {
    return false;
  }

  const separatorIndex = projectKey.indexOf(":");
  if (separatorIndex <= 0 || separatorIndex >= projectKey.length - 1) {
    return false;
  }

  const environmentId = projectKey.slice(0, separatorIndex);
  const projectId = projectKey.slice(separatorIndex + 1);
  return (
    environmentId.length <= ACTIVITY_DOCK_PROJECT_KEY_PART_MAX_LENGTH &&
    projectId.length <= ACTIVITY_DOCK_PROJECT_KEY_PART_MAX_LENGTH &&
    environmentId.trim() === environmentId &&
    projectId.trim() === projectId
  );
}

export function sanitizePersistedActivityDockState(persistedState: unknown): {
  expandedByProject: Record<string, boolean>;
} {
  if (!isRecord(persistedState) || !isRecord(persistedState.expandedByProject)) {
    return { expandedByProject: {} };
  }

  const entries: Array<[string, boolean]> = [];
  for (const [projectKey, expanded] of Object.entries(persistedState.expandedByProject)) {
    if (entries.length >= ACTIVITY_DOCK_MAX_PROJECTS) {
      break;
    }
    if (typeof expanded !== "boolean" || !isCanonicalProjectKey(projectKey)) {
      continue;
    }
    entries.push([projectKey, expanded]);
  }
  return { expandedByProject: Object.fromEntries(entries) };
}

function canAddProject(expandedByProject: Record<string, boolean>, projectKey: string): boolean {
  return (
    Object.hasOwn(expandedByProject, projectKey) ||
    Object.keys(expandedByProject).length < ACTIVITY_DOCK_MAX_PROJECTS
  );
}

export const useActivityDockStore = create<ActivityDockStoreState>()(
  persist(
    (set) => ({
      expandedByProject: {},
      setExpanded: (projectKey, expanded) => {
        if (typeof expanded !== "boolean" || !isCanonicalProjectKey(projectKey)) {
          return;
        }
        set((state) => {
          if (
            !canAddProject(state.expandedByProject, projectKey) ||
            state.expandedByProject[projectKey] === expanded
          ) {
            return state;
          }
          return {
            expandedByProject: {
              ...state.expandedByProject,
              [projectKey]: expanded,
            },
          };
        });
      },
      toggleExpanded: (projectKey) => {
        if (!isCanonicalProjectKey(projectKey)) {
          return;
        }
        set((state) => {
          if (!canAddProject(state.expandedByProject, projectKey)) {
            return state;
          }
          return {
            expandedByProject: {
              ...state.expandedByProject,
              [projectKey]: state.expandedByProject[projectKey] !== true,
            },
          };
        });
      },
    }),
    {
      name: ACTIVITY_DOCK_STORAGE_KEY,
      version: ACTIVITY_DOCK_STORAGE_VERSION,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => sanitizePersistedActivityDockState(state),
      migrate: sanitizePersistedActivityDockState,
      merge: (persistedState, currentState) => {
        if (persistedState === undefined) {
          return currentState;
        }
        return {
          ...currentState,
          ...sanitizePersistedActivityDockState(persistedState),
        };
      },
    },
  ),
);

export function selectActivityDockExpanded(
  expandedByProject: Readonly<Record<string, boolean>>,
  projectKey: string,
): boolean {
  return expandedByProject[projectKey] === true;
}
