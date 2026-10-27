import type {
  DesktopBridge,
  DesktopProjectDataEnvironmentStatus,
  DesktopProjectDataRecoveryResult,
} from "@coddy/contracts";
import { useSyncExternalStore } from "react";

type ProjectDataBridge = Pick<
  DesktopBridge,
  | "getProjectDataStatuses"
  | "restoreProjectData"
  | "startEmptyProjectData"
  | "retryProjectData"
  | "openProjectDataPath"
  | "exportProjectDataDiagnostics"
>;

export type ProjectDataRecoveryTrigger = "automatic" | "manual";

export interface ProjectDataSafetySnapshot {
  readonly open: boolean;
  readonly trigger: ProjectDataRecoveryTrigger;
  readonly environmentId: string | null;
  readonly selected: DesktopProjectDataEnvironmentStatus | null;
  readonly statuses: readonly DesktopProjectDataEnvironmentStatus[];
  readonly busy: boolean;
  readonly error: string | null;
  readonly lastResult: DesktopProjectDataRecoveryResult | null;
  readonly requiresStorageAdoption: boolean;
}

const INITIAL_SNAPSHOT: ProjectDataSafetySnapshot = Object.freeze({
  open: false,
  trigger: "manual",
  environmentId: null,
  selected: null,
  statuses: [],
  busy: false,
  error: null,
  lastResult: null,
  requiresStorageAdoption: false,
});

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Project data recovery failed.";
}

function requireMethod<K extends keyof ProjectDataBridge>(
  bridge: ProjectDataBridge | undefined,
  method: K,
): NonNullable<ProjectDataBridge[K]> {
  const operation = bridge?.[method];
  if (typeof operation !== "function") {
    throw new Error("Project data recovery requires the current Coddy Studio desktop application.");
  }
  return operation as NonNullable<ProjectDataBridge[K]>;
}

export function createProjectDataSafetyStore(getBridge: () => ProjectDataBridge | undefined) {
  let snapshot = INITIAL_SNAPSHOT;
  const listeners = new Set<() => void>();

  const publish = (patch: Partial<ProjectDataSafetySnapshot>) => {
    snapshot = Object.freeze({ ...snapshot, ...patch });
    for (const listener of listeners) listener();
  };

  const refresh = async () => {
    const environmentId = snapshot.environmentId;
    if (environmentId === null) return;
    publish({ busy: true, error: null });
    try {
      const statuses = await requireMethod(getBridge(), "getProjectDataStatuses")();
      publish({
        statuses,
        selected: statuses.find((status) => status.environmentId === environmentId) ?? null,
        busy: false,
      });
    } catch (error) {
      publish({ busy: false, error: errorMessage(error) });
      throw error;
    }
  };

  const open = async (environmentId: string, trigger: ProjectDataRecoveryTrigger) => {
    publish({
      open: true,
      trigger,
      environmentId,
      selected: null,
      error: null,
      lastResult: null,
      requiresStorageAdoption: false,
    });
    await refresh();
  };

  const runRecovery = async (
    action: "restore" | "start-empty",
    backupId?: string,
  ): Promise<DesktopProjectDataRecoveryResult> => {
    const environmentId = snapshot.environmentId;
    if (environmentId === null) throw new Error("No project data environment is selected.");
    publish({ busy: true, error: null });
    try {
      const bridge = getBridge();
      const result =
        action === "restore"
          ? await requireMethod(bridge, "restoreProjectData")(environmentId, backupId ?? "")
          : await requireMethod(bridge, "startEmptyProjectData")(environmentId);
      publish({
        busy: false,
        lastResult: result,
        requiresStorageAdoption: action === "start-empty",
      });
      return result;
    } catch (error) {
      publish({ busy: false, error: errorMessage(error) });
      throw error;
    }
  };

  return {
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    open,
    close: () => publish({ open: false }),
    refresh,
    retry: async () => {
      const environmentId = snapshot.environmentId;
      if (environmentId === null) return;
      const previousResult = snapshot.lastResult;
      publish({ busy: true, error: null });
      try {
        await requireMethod(getBridge(), "retryProjectData")(environmentId);
        publish({
          busy: false,
          lastResult: previousResult === null ? null : { ...previousResult, restartError: null },
        });
        await refresh();
      } catch (error) {
        publish({ busy: false, error: errorMessage(error) });
        throw error;
      }
    },
    restore: (backupId: string) => runRecovery("restore", backupId),
    startEmpty: () => runRecovery("start-empty"),
    openPath: async () => {
      const environmentId = snapshot.environmentId;
      if (environmentId === null) return;
      await requireMethod(getBridge(), "openProjectDataPath")(environmentId);
    },
    exportDiagnostics: async () => {
      const environmentId = snapshot.environmentId;
      if (environmentId === null) return null;
      return requireMethod(getBridge(), "exportProjectDataDiagnostics")(environmentId);
    },
  };
}

export const projectDataSafetyStore = createProjectDataSafetyStore(() =>
  typeof window === "undefined" ? undefined : window.desktopBridge,
);

export function useProjectDataSafetySnapshot(): ProjectDataSafetySnapshot {
  return useSyncExternalStore(
    projectDataSafetyStore.subscribe,
    projectDataSafetyStore.getSnapshot,
    projectDataSafetyStore.getSnapshot,
  );
}
