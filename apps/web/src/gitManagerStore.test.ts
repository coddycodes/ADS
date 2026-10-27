import { projectKey } from "@coddy/client-runtime/state/entities";
import type { ScopedProjectRef } from "@coddy/contracts";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import { createJSONStorage } from "zustand/middleware";

import { GIT_MANAGER_STORAGE_KEY, useGitManagerStore } from "./gitManagerStore";

const ref = (environmentId: string, projectId: string) =>
  ({ environmentId, projectId }) as ScopedProjectRef;

const persisted = new Map<string, string>();
const storage = {
  getItem: (name: string) => persisted.get(name) ?? null,
  setItem: (name: string, value: string) => {
    persisted.set(name, value);
  },
  removeItem: (name: string) => {
    persisted.delete(name);
  },
};

describe("gitManagerStore", () => {
  beforeEach(() => {
    persisted.clear();
    useGitManagerStore.persist.setOptions({ storage: createJSONStorage(() => storage) });
    useGitManagerStore.setState({ byProjectKey: {} });
  });

  it("evicts the least recently used project when a third is touched", () => {
    const store = useGitManagerStore.getState();
    store.touchProject(ref("env-a", "p1"));
    store.touchProject(ref("env-a", "p2"));
    store.touchProject(ref("env-a", "p3"));

    const keys = Object.keys(useGitManagerStore.getState().byProjectKey);
    expect(keys).toHaveLength(2);
    expect(keys).not.toContain(projectKey(ref("env-a", "p1")));
  });

  it("keys by environment and project, not by bare project id", () => {
    const store = useGitManagerStore.getState();
    store.setActiveTab(ref("env-a", "p1"), "changes");

    expect(store.selectViewState(ref("env-b", "p1")).activeTab).toBe("history");
  });

  it("rehydrates only the two most recently used projects", async () => {
    const store = useGitManagerStore.getState();
    store.touchProject(ref("env-a", "p1"));
    store.touchProject(ref("env-a", "p2"));
    store.touchProject(ref("env-a", "p3"));
    store.setActiveTab(ref("env-a", "p2"), "changes");
    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    expect(serialized).toBeDefined();

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(Object.keys(useGitManagerStore.getState().byProjectKey)).toHaveLength(2);
    expect(useGitManagerStore.getState().selectViewState(ref("env-a", "p1"))).toMatchObject({
      activeTab: "history",
      lastUsedAt: 0,
    });
    // The active tab is session-only: a rehydrated project opens on History.
    expect(useGitManagerStore.getState().selectViewState(ref("env-a", "p2")).activeTab).toBe(
      "history",
    );
    expect(useGitManagerStore.getState().byProjectKey).toHaveProperty(
      projectKey(ref("env-a", "p3")),
    );
  });

  it("round-trips the commit draft through the versioned project key", async () => {
    const project = ref("env-a", "p1");
    useGitManagerStore.getState().setCommitDraft(project, "Keep this message");
    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    const parsed = JSON.parse(serialized!) as {
      state: { byProjectKey: Record<string, { commitDraft: string }> };
    };
    expect(parsed.state.byProjectKey[projectKey(project)]?.commitDraft).toBe("Keep this message");

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(useGitManagerStore.getState().selectViewState(project).commitDraft).toBe(
      "Keep this message",
    );
  });

  it("persists stash selection by sha and the stash pane state", async () => {
    const project = ref("env-a", "p1");
    const store = useGitManagerStore.getState();
    store.setSelectedStash(project, "stable-stash-sha");
    store.setStashPaneOpen(project, true);
    store.setTagSectionCollapsed(project, "remote:origin", true);
    store.setTagSectionCollapsed(project, "local", true);
    store.setTagSectionCollapsed(project, "local", false);

    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    const parsed = JSON.parse(serialized!) as {
      state: {
        byProjectKey: Record<string, { selectedStashSha: string | null; stashPaneOpen: boolean }>;
      };
    };
    expect(parsed.state.byProjectKey[projectKey(project)]).toMatchObject({
      selectedStashSha: "stable-stash-sha",
      stashPaneOpen: true,
    });
    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(useGitManagerStore.getState().selectViewState(project)).toMatchObject({
      selectedStashSha: "stable-stash-sha",
      stashPaneOpen: true,
      collapsedTagSections: ["remote:origin"],
      selectedWorktreeCwd: null,
    });
  });

  it("persists JSON-safe line selections per path with sorted unique indices", async () => {
    const project = ref("env-a", "p1");
    useGitManagerStore.getState().setLineSelection(project, "src/file.ts", {
      type: "partial",
      basis: "none",
      diverging: [5, 1, 5, 3],
      selectable: [5, 3, 1],
      area: "unstaged",
      generation: 42,
    });

    expect(useGitManagerStore.getState().selectViewState(project).lineSelectionByPath).toEqual({
      "src/file.ts": {
        type: "partial",
        basis: "none",
        diverging: [1, 3, 5],
        selectable: [1, 3, 5],
        area: "unstaged",
        generation: 42,
      },
    });
    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    expect(serialized).toBeDefined();

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(useGitManagerStore.getState().selectViewState(project).lineSelectionByPath).toEqual({
      "src/file.ts": expect.objectContaining({ diverging: [1, 3, 5], generation: 42 }),
    });
  });

  it("removes a path selection without repersisting checkout selection", () => {
    const project = ref("env-a", "p1");
    const store = useGitManagerStore.getState();
    store.setSelectedWorktree(project, "/opaque/worktree");
    store.setLineSelection(project, "src/file.ts", {
      type: "all",
      basis: "all",
      diverging: [],
      selectable: [0],
      area: "staged",
      generation: 7,
    });
    store.setLineSelection(project, "src/file.ts", null);

    expect(store.selectViewState(project).lineSelectionByPath).toEqual({});
    const serialized = JSON.parse(persisted.get(GIT_MANAGER_STORAGE_KEY)!) as {
      state: { byProjectKey: Record<string, Record<string, unknown>> };
    };
    expect(serialized.state.byProjectKey[projectKey(project)]).not.toHaveProperty(
      "selectedWorktreeCwd",
    );
  });

  it("keeps checkout selection and the tab in memory while persisting the commit draft", async () => {
    const project = ref("env-a", "p1");
    const store = useGitManagerStore.getState();
    store.setSelectedWorktree(project, "/opaque/feature");
    store.setActiveTab(project, "changes");
    store.setCommitDraft(project, "Keep this draft");

    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    const parsed = JSON.parse(serialized!) as {
      state: { byProjectKey: Record<string, Record<string, unknown>> };
    };
    const persistedViewState = parsed.state.byProjectKey[projectKey(project)];
    expect(persistedViewState).not.toHaveProperty("selectedWorktreeCwd");
    expect(persistedViewState).not.toHaveProperty("activeTab");
    expect(persistedViewState).toMatchObject({ commitDraft: "Keep this draft" });

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(useGitManagerStore.getState().selectViewState(project)).toMatchObject({
      selectedWorktreeCwd: null,
      activeTab: "history",
      commitDraft: "Keep this draft",
    });
  });

  it("sanitizes current-version persisted state and reapplies the LRU bound", async () => {
    persisted.set(
      GIT_MANAGER_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        state: {
          byProjectKey: {
            invalid: { activeTab: "history", lastUsedAt: 100 },
            [projectKey(ref("env-a", "p1"))]: { activeTab: "invalid", lastUsedAt: 1 },
            [projectKey(ref("env-a", "p2"))]: { activeTab: "history", lastUsedAt: 2 },
            [projectKey(ref("env-a", "p3"))]: { activeTab: "history", lastUsedAt: 3 },
          },
        },
      }),
    );

    await useGitManagerStore.persist.rehydrate();

    const state = useGitManagerStore.getState();
    expect(Object.keys(state.byProjectKey)).toEqual([
      projectKey(ref("env-a", "p3")),
      projectKey(ref("env-a", "p2")),
    ]);
    expect(state.selectViewState(ref("env-a", "p1"))).toEqual(
      expect.objectContaining({ activeTab: "history", lastUsedAt: 0 }),
    );
  });

  it("persists a sanitized per-project multi-commit selection without persisting the checkout", async () => {
    const project = ref("env-a", "p1");
    const store = useGitManagerStore.getState();
    store.setSelectedWorktree(project, "/opaque/worktree");
    store.setMultiCommitSelection(project, ["commit-b", "", "commit-a", "commit-b"]);

    expect(store.selectViewState(project).multiCommitSelection).toEqual(["commit-b", "commit-a"]);
    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    const parsed = JSON.parse(serialized!) as {
      state: { byProjectKey: Record<string, Record<string, unknown>> };
    };
    expect(parsed.state.byProjectKey[projectKey(project)]).toMatchObject({
      multiCommitSelection: ["commit-b", "commit-a"],
    });
    expect(parsed.state.byProjectKey[projectKey(project)]).not.toHaveProperty(
      "selectedWorktreeCwd",
    );

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();

    expect(useGitManagerStore.getState().selectViewState(project)).toMatchObject({
      selectedWorktreeCwd: null,
      multiCommitSelection: ["commit-b", "commit-a"],
    });
  });

  it("persists only the requested image mode and provider-pane fields with project view state", async () => {
    const project = ref("env-a", "p1");
    const store = useGitManagerStore.getState();
    store.setSelectedWorktree(project, "/opaque/worktree");
    store.setImageDiffMode(project, "onion");
    store.setProviderPaneOpen(project, true);

    const serialized = persisted.get(GIT_MANAGER_STORAGE_KEY);
    const parsed = JSON.parse(serialized!) as {
      state: { byProjectKey: Record<string, Record<string, unknown>> };
    };
    expect(parsed.state.byProjectKey[projectKey(project)]).toMatchObject({
      imageDiffMode: "onion",
      providerPaneOpen: true,
    });
    expect(parsed.state.byProjectKey[projectKey(project)]).not.toHaveProperty(
      "selectedWorktreeCwd",
    );

    useGitManagerStore.setState({ byProjectKey: {} });
    persisted.set(GIT_MANAGER_STORAGE_KEY, serialized!);
    await useGitManagerStore.persist.rehydrate();
    expect(useGitManagerStore.getState().selectViewState(project)).toMatchObject({
      selectedWorktreeCwd: null,
      imageDiffMode: "onion",
      providerPaneOpen: true,
    });
  });
});
