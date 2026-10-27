import type { DesktopBridge, DesktopProjectDataEnvironmentStatus } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import { createProjectDataSafetyStore } from "./projectDataSafety";

const status: DesktopProjectDataEnvironmentStatus = {
  environmentId: "primary",
  label: "Local",
  runningDistro: null,
  status: "recovery-required",
  requestedRoot: "/Users/user/.coddy/studio",
  effectiveRoot: "/Users/user/.coddy/studio",
  isFilesystemAlias: false,
  storageInstanceId: "b102f72a-c63b-4801-8f14-fba7a16856b8",
  issue: "The database is missing.",
  backups: [],
};

describe("projectDataSafetyStore", () => {
  it("loads the requested environment and retries the exact registered backend before inspection", async () => {
    const getProjectDataStatuses = vi.fn(async () => [status]);
    const retryProjectData = vi.fn(async () => undefined);
    const bridge = { getProjectDataStatuses, retryProjectData } as Pick<
      DesktopBridge,
      "getProjectDataStatuses" | "retryProjectData"
    >;
    const store = createProjectDataSafetyStore(() => bridge);

    await store.open("primary", "automatic");
    expect(store.getSnapshot()).toMatchObject({
      open: true,
      trigger: "automatic",
      selected: status,
    });

    await store.retry();
    expect(retryProjectData).toHaveBeenCalledWith("primary");
    expect(getProjectDataStatuses).toHaveBeenCalledTimes(2);
  });

  it("uses exact identifiers for restore and routes start-empty through adoption", async () => {
    const restoreProjectData = vi.fn(async () => ({
      environmentId: "primary",
      action: "restore" as const,
      committed: true,
      preservedDirectory: "preserved",
      storageInstanceId: status.storageInstanceId,
      restartError: null,
    }));
    const startEmptyProjectData = vi.fn(async () => ({
      environmentId: "primary",
      action: "start-empty" as const,
      committed: true,
      preservedDirectory: "preserved",
      storageInstanceId: null,
      restartError: "The backend did not restart.",
    }));
    const retryProjectData = vi.fn(async () => undefined);
    const bridge = {
      getProjectDataStatuses: vi.fn(async () => [status]),
      restoreProjectData,
      startEmptyProjectData,
      retryProjectData,
      openProjectDataPath: vi.fn(async () => undefined),
      exportProjectDataDiagnostics: vi.fn(async () => "diagnostics.json"),
    } as Pick<
      DesktopBridge,
      | "getProjectDataStatuses"
      | "restoreProjectData"
      | "startEmptyProjectData"
      | "retryProjectData"
      | "openProjectDataPath"
      | "exportProjectDataDiagnostics"
    >;
    const store = createProjectDataSafetyStore(() => bridge);
    await store.open("primary", "manual");

    await store.restore("26b6ca53-27d3-401a-b51f-d7bdf534081f");
    expect(restoreProjectData).toHaveBeenCalledWith(
      "primary",
      "26b6ca53-27d3-401a-b51f-d7bdf534081f",
    );
    expect(store.getSnapshot().requiresStorageAdoption).toBe(false);

    await store.startEmpty();
    expect(startEmptyProjectData).toHaveBeenCalledWith("primary");
    expect(store.getSnapshot().requiresStorageAdoption).toBe(true);
    expect(store.getSnapshot().lastResult?.restartError).toBe("The backend did not restart.");

    await store.retry();
    expect(retryProjectData).toHaveBeenCalledWith("primary");
    expect(store.getSnapshot().lastResult?.restartError).toBeNull();
  });
});
