import {
  scopedProjectKey,
  scopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@coddy/client-runtime/environment";
import { EnvironmentId, ProjectId, ThreadId } from "@coddy/contracts";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import { createJSONStorage } from "zustand/middleware";

import {
  ACTIVITY_DOCK_MAX_PROJECTS,
  ACTIVITY_DOCK_PROJECT_KEY_MAX_LENGTH,
  ACTIVITY_DOCK_STORAGE_KEY,
  sanitizePersistedActivityDockState,
  selectActivityDockExpanded,
  useActivityDockStore,
} from "./activityDockStore";

const environmentA = EnvironmentId.make("env-a");
const environmentB = EnvironmentId.make("env-b");
const projectA = ProjectId.make("project-a");
const projectB = ProjectId.make("project-b");
const projectKeyA = scopedProjectKey(scopeProjectRef(environmentA, projectA));
const projectKeyB = scopedProjectKey(scopeProjectRef(environmentA, projectB));
const projectKeyAOtherEnvironment = scopedProjectKey(scopeProjectRef(environmentB, projectA));
const nonStringProjectKeys: ReadonlyArray<readonly [label: string, value: unknown]> = [
  ["undefined", undefined],
  ["null", null],
  ["object", {}],
  ["array", []],
  ["number", 42],
];

function withRecordingStorage(assertions: () => void): void {
  const originalStorage = useActivityDockStore.persist.getOptions().storage;
  expect(originalStorage).toBeDefined();
  const serializedStorage = new Map<string, string>();
  let writeCount = 0;
  const recordingStorage = createJSONStorage(() => ({
    getItem: (name: string) => serializedStorage.get(name) ?? null,
    setItem: (name: string, value: string) => {
      writeCount += 1;
      serializedStorage.set(name, value);
    },
    removeItem: (name: string) => {
      serializedStorage.delete(name);
    },
  }));
  expect(recordingStorage).toBeDefined();

  try {
    useActivityDockStore.persist.setOptions({
      storage: recordingStorage as NonNullable<typeof originalStorage>,
    });
    useActivityDockStore.setState({
      expandedByProject: {
        [projectKeyA]: true,
      },
    });
    const persistedBeforeAction = serializedStorage.get(ACTIVITY_DOCK_STORAGE_KEY);
    writeCount = 0;

    assertions();

    expect(useActivityDockStore.getState().expandedByProject).toEqual({
      [projectKeyA]: true,
    });
    expect(serializedStorage.get(ACTIVITY_DOCK_STORAGE_KEY)).toBe(persistedBeforeAction);
    expect(writeCount).toBe(0);
  } finally {
    useActivityDockStore.persist.setOptions({
      storage: originalStorage,
    });
    useActivityDockStore.setState({ expandedByProject: {} });
  }
}

beforeEach(() => {
  useActivityDockStore.setState({ expandedByProject: {} });
});

describe("activityDockStore", () => {
  it("defaults every project to collapsed", () => {
    expect(useActivityDockStore.getState().expandedByProject).toEqual({});
    expect(selectActivityDockExpanded({}, projectKeyA)).toBe(false);
  });

  it("keys expansion by scoped environment and project, never by thread", () => {
    const threadKey = scopedThreadKey(
      scopeThreadRef(environmentA, ThreadId.make("thread-within-project-a")),
    );

    useActivityDockStore.getState().setExpanded(projectKeyA, true);

    expect(useActivityDockStore.getState().expandedByProject).toEqual({
      [projectKeyA]: true,
    });
    expect(useActivityDockStore.getState().expandedByProject).not.toHaveProperty(threadKey);
  });

  it("isolates toggles across projects and environments", () => {
    useActivityDockStore.getState().toggleExpanded(projectKeyA);

    expect(
      selectActivityDockExpanded(useActivityDockStore.getState().expandedByProject, projectKeyA),
    ).toBe(true);
    expect(
      selectActivityDockExpanded(useActivityDockStore.getState().expandedByProject, projectKeyB),
    ).toBe(false);
    expect(
      selectActivityDockExpanded(
        useActivityDockStore.getState().expandedByProject,
        projectKeyAOtherEnvironment,
      ),
    ).toBe(false);

    useActivityDockStore.getState().toggleExpanded(projectKeyAOtherEnvironment);
    useActivityDockStore.getState().toggleExpanded(projectKeyA);

    expect(useActivityDockStore.getState().expandedByProject).toEqual({
      [projectKeyA]: false,
      [projectKeyAOtherEnvironment]: true,
    });
  });

  it("rejects invalid action keys and does not evict existing entries at the cap", () => {
    useActivityDockStore.getState().setExpanded("not-scoped", true);
    useActivityDockStore.getState().setExpanded(`env:${"p".repeat(257)}`, true);
    useActivityDockStore
      .getState()
      .setExpanded(`e${"x".repeat(ACTIVITY_DOCK_PROJECT_KEY_MAX_LENGTH)}:p`, true);

    for (let index = 0; index < ACTIVITY_DOCK_MAX_PROJECTS; index += 1) {
      useActivityDockStore.getState().setExpanded(`env:project-${index}`, true);
    }
    useActivityDockStore.getState().setExpanded("env:overflow", true);
    useActivityDockStore.getState().setExpanded("env:project-0", false);
    useActivityDockStore.getState().toggleExpanded("env:overflow");

    const expandedByProject = useActivityDockStore.getState().expandedByProject;
    expect(Object.keys(expandedByProject)).toHaveLength(ACTIVITY_DOCK_MAX_PROJECTS);
    expect(expandedByProject["env:project-0"]).toBe(false);
    expect(expandedByProject).not.toHaveProperty("env:overflow");
    expect(expandedByProject).not.toHaveProperty("not-scoped");
  });

  it("rejects non-boolean expansion values at the runtime boundary", () => {
    useActivityDockStore.getState().setExpanded(projectKeyA, "true" as never);

    expect(useActivityDockStore.getState().expandedByProject).toEqual({});
  });

  describe.each(nonStringProjectKeys)("non-string project key: %s", (_label, projectKey) => {
    it("makes setExpanded a total no-op without a persistence write", () => {
      withRecordingStorage(() => {
        expect(() =>
          useActivityDockStore.getState().setExpanded(projectKey as never, false),
        ).not.toThrow();
      });
    });

    it("makes toggleExpanded a total no-op without a persistence write", () => {
      withRecordingStorage(() => {
        expect(() =>
          useActivityDockStore.getState().toggleExpanded(projectKey as never),
        ).not.toThrow();
      });
    });
  });

  it("totally sanitizes corrupt persisted input and bounds retained entries", () => {
    expect(sanitizePersistedActivityDockState(undefined)).toEqual({ expandedByProject: {} });
    expect(sanitizePersistedActivityDockState(null)).toEqual({ expandedByProject: {} });
    expect(sanitizePersistedActivityDockState([])).toEqual({ expandedByProject: {} });
    expect(
      sanitizePersistedActivityDockState({
        expandedByProject: "corrupt",
        counts: { active: 99 },
      }),
    ).toEqual({ expandedByProject: {} });

    const expandedByProject = Object.fromEntries([
      ...Array.from({ length: ACTIVITY_DOCK_MAX_PROJECTS + 2 }, (_, index) => [
        `env:project-${index}`,
        index % 2 === 0,
      ]),
      ["env:not-a-boolean", "true"],
      ["not-scoped", true],
      [`env:${"p".repeat(257)}`, true],
    ]);

    const sanitized = sanitizePersistedActivityDockState({
      expandedByProject,
      route: { selectedRecordId: "actor-1" },
      provider: { actors: [{ id: "actor-1" }] },
    });

    expect(Object.keys(sanitized.expandedByProject)).toHaveLength(ACTIVITY_DOCK_MAX_PROJECTS);
    expect(sanitized.expandedByProject["env:project-0"]).toBe(true);
    expect(sanitized.expandedByProject[`env:project-${ACTIVITY_DOCK_MAX_PROJECTS - 1}`]).toBe(
      false,
    );
    expect(sanitized.expandedByProject).not.toHaveProperty(
      `env:project-${ACTIVITY_DOCK_MAX_PROJECTS}`,
    );
    expect(sanitized).toEqual({
      expandedByProject: Object.fromEntries(
        Array.from({ length: ACTIVITY_DOCK_MAX_PROJECTS }, (_, index) => [
          `env:project-${index}`,
          index % 2 === 0,
        ]),
      ),
    });
  });

  it("persists only canonical boolean project preferences", () => {
    const persistOptions = useActivityDockStore.persist.getOptions();
    expect(persistOptions.name).toBe(ACTIVITY_DOCK_STORAGE_KEY);
    expect(persistOptions.partialize).toBeDefined();

    const partialized = persistOptions.partialize?.({
      ...useActivityDockStore.getState(),
      expandedByProject: {
        [projectKeyA]: true,
        [projectKeyB]: false,
        "env:bad-value": "true",
        "not-scoped": true,
      },
      counts: { active: 42 },
      route: { selectedRecordId: "actor-1" },
      provider: { secret: true },
    } as never);

    expect(partialized).toEqual({
      expandedByProject: {
        [projectKeyA]: true,
        [projectKeyB]: false,
      },
    });
  });

  it("validates and bounds same-version hydration before merging it", async () => {
    const originalStorage = useActivityDockStore.persist.getOptions().storage;
    expect(originalStorage).toBeDefined();
    const persistedEntries = Object.fromEntries(
      Array.from({ length: ACTIVITY_DOCK_MAX_PROJECTS + 1 }, (_, index) => [
        `env:hydrated-${index}`,
        index % 2 === 0,
      ]),
    );
    const serializedStorage = new Map([
      [
        ACTIVITY_DOCK_STORAGE_KEY,
        JSON.stringify({
          state: {
            expandedByProject: {
              [projectKeyA]: true,
              [projectKeyB]: "true",
              "not-scoped": true,
              ...persistedEntries,
            },
            counts: { active: 5, done: 9 },
            scopeId: "thread-1",
            provider: { token: "do-not-retain" },
            injectedTopLevel: true,
          },
          version: 1,
        }),
      ],
    ]);
    const hydrationStorage = createJSONStorage(() => ({
      getItem: (name: string) => serializedStorage.get(name) ?? null,
      setItem: (name: string, value: string) => {
        serializedStorage.set(name, value);
      },
      removeItem: (name: string) => {
        serializedStorage.delete(name);
      },
    }));
    expect(hydrationStorage).toBeDefined();

    try {
      useActivityDockStore.persist.setOptions({
        storage: hydrationStorage as NonNullable<typeof originalStorage>,
      });
      await useActivityDockStore.persist.rehydrate();

      const hydrated = useActivityDockStore.getState();
      expect(hydrated).not.toHaveProperty("counts");
      expect(hydrated).not.toHaveProperty("scopeId");
      expect(hydrated).not.toHaveProperty("provider");
      expect(hydrated).not.toHaveProperty("injectedTopLevel");
      expect(typeof hydrated.setExpanded).toBe("function");
      expect(typeof hydrated.toggleExpanded).toBe("function");
      expect(Object.keys(hydrated.expandedByProject)).toHaveLength(ACTIVITY_DOCK_MAX_PROJECTS);
      expect(hydrated.expandedByProject[projectKeyA]).toBe(true);
      expect(hydrated.expandedByProject).not.toHaveProperty(projectKeyB);
      expect(hydrated.expandedByProject).not.toHaveProperty("not-scoped");
      expect(hydrated.expandedByProject).not.toHaveProperty(
        `env:hydrated-${ACTIVITY_DOCK_MAX_PROJECTS - 1}`,
      );
    } finally {
      useActivityDockStore.persist.setOptions({
        storage: originalStorage,
      });
      useActivityDockStore.setState({ expandedByProject: {} });
    }
  });
});
