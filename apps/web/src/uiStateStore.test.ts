import { ProjectId, ThreadId } from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  legacyProjectCwdPreferenceKey,
  markThreadUnread,
  markThreadVisited,
  parsePersistedState,
  PERSISTED_STATE_KEY,
  type PersistedUiState,
  persistState,
  reorderProjects,
  resolveAgentsGroupExpanded,
  resolveProjectExpanded,
  setAgentsGroupExpanded,
  setAgentsSectionExpanded,
  setDefaultAdvertisedEndpointKey,
  setProjectExpanded,
  setRepositoryGroupExpanded,
  setSidebarView,
  setThreadChangedFilesExpanded,
  type UiState,
} from "./uiStateStore";

function makeUiState(overrides: Partial<UiState> = {}): UiState {
  return {
    agentsSectionExpanded: true,
    agentsGroupExpandedById: {},
    sidebarView: "environments",
    repositoryGroupExpandedById: {},
    projectExpandedById: {},
    projectOrder: [],
    threadLastVisitedAtById: {},
    threadChangedFilesExpandedById: {},
    defaultAdvertisedEndpointKey: null,
    ...overrides,
  };
}

const initialUiState = parsePersistedState({});

describe("agents section expansion", () => {
  it("defaults the section expanded and the done group collapsed", () => {
    expect(initialUiState.agentsSectionExpanded).toBe(true);
    expect(resolveAgentsGroupExpanded({}, "done")).toBe(false);
    expect(resolveAgentsGroupExpanded({}, "status:done")).toBe(false);
    expect(resolveAgentsGroupExpanded({}, "working")).toBe(true);
    expect(resolveAgentsGroupExpanded({}, "project:Done")).toBe(true);
    expect(resolveAgentsGroupExpanded({ done: true }, "done")).toBe(true);
    expect(resolveAgentsGroupExpanded({ "status:done": true }, "status:done")).toBe(true);
  });

  it("updates immutably and no-ops on same value", () => {
    const collapsed = setAgentsSectionExpanded(initialUiState, false);
    expect(collapsed.agentsSectionExpanded).toBe(false);
    expect(setAgentsSectionExpanded(collapsed, false)).toBe(collapsed);
    const next = setAgentsGroupExpanded(initialUiState, "done", true);
    expect(next.agentsGroupExpandedById).toEqual({ done: true });
    expect(setAgentsGroupExpanded(next, "done", true)).toBe(next);
  });
});

describe("sidebar view preference", () => {
  it("defaults to environments and restores persisted values", () => {
    expect(parsePersistedState({}).sidebarView).toBe("environments");
    expect(parsePersistedState({ sidebarView: "bogus" as never }).sidebarView).toBe("environments");
    const restored = parsePersistedState({
      sidebarView: "repositories",
      repositoryGroupExpandedById: { "github.com/acme/repo": false },
    });
    expect(restored.sidebarView).toBe("repositories");
    expect(restored.repositoryGroupExpandedById).toEqual({ "github.com/acme/repo": false });
  });

  it("updates without copying unchanged state", () => {
    const state = parsePersistedState({});
    expect(setSidebarView(state, "environments")).toBe(state);
    expect(setSidebarView(state, "repositories").sidebarView).toBe("repositories");
    const collapsed = setRepositoryGroupExpanded(state, "key", false);
    expect(collapsed.repositoryGroupExpandedById).toEqual({ key: false });
    expect(setRepositoryGroupExpanded(collapsed, "key", false)).toBe(collapsed);
  });
});

describe("uiStateStore pure functions", () => {
  it("stores server timestamps without moving visit state backwards", () => {
    const threadId = ThreadId.make("thread-1");
    const initialState = makeUiState();
    const visited = markThreadVisited(initialState, threadId, "2026-02-25T12:30:00.700Z");

    expect(visited.threadLastVisitedAtById[threadId]).toBe("2026-02-25T12:30:00.700Z");
    expect(markThreadVisited(visited, threadId, "2026-02-25T12:30:00.000Z")).toBe(visited);
    expect(markThreadVisited(visited, threadId, "not-a-date")).toBe(visited);
  });

  it("marks a completed thread unread using the server completion timestamp", () => {
    const threadId = ThreadId.make("thread-1");
    const initialState = makeUiState({
      threadLastVisitedAtById: {
        [threadId]: "2026-02-25T12:35:00.000Z",
      },
    });

    const next = markThreadUnread(initialState, threadId, "2026-02-25T12:30:00.000Z");

    expect(next.threadLastVisitedAtById[threadId]).toBe("2026-02-25T12:29:59.999Z");
    expect(markThreadUnread(next, threadId, null)).toBe(next);
    expect(markThreadUnread(next, threadId, "not-a-date")).toBe(next);
    expect(markThreadUnread(next, threadId, "2026-02-25T12:30:00.000Z")).toBe(next);
  });

  it("resolves project expansion from logical, physical, and legacy preference keys", () => {
    const physicalKey = "environment:/repo/project";
    const legacyKey = legacyProjectCwdPreferenceKey("/repo/project");

    expect(resolveProjectExpanded({ logical: false, [physicalKey]: true }, ["logical"])).toBe(
      false,
    );
    expect(resolveProjectExpanded({ [physicalKey]: false }, ["new-logical", physicalKey])).toBe(
      false,
    );
    expect(resolveProjectExpanded({ [legacyKey]: false }, ["new-logical", legacyKey])).toBe(false);
    expect(resolveProjectExpanded({}, ["new-logical"])).toBe(true);
  });

  it("sets expansion for every stable key belonging to a logical project", () => {
    const initialState = makeUiState();
    const keys = ["logical", "environment-a:/repo", "environment-b:/repo"];

    const next = setProjectExpanded(initialState, keys, false);

    expect(next.projectExpandedById).toEqual({
      logical: false,
      "environment-a:/repo": false,
      "environment-b:/repo": false,
    });
    expect(setProjectExpanded(next, keys, false)).toBe(next);
    expect(setProjectExpanded(next, "logical", true).projectExpandedById.logical).toBe(true);
  });

  it("reorders from the current atom-derived project order", () => {
    const project1 = ProjectId.make("project-1");
    const project2 = ProjectId.make("project-2");
    const project3 = ProjectId.make("project-3");
    const currentOrder = [project1, project2, project3];

    const next = reorderProjects(makeUiState(), currentOrder, [project1], [project3]);

    expect(next.projectOrder).toEqual([project2, project3, project1]);
  });

  it("moves grouped project members together", () => {
    const keyALocal = "env-local:proj-a";
    const keyARemote = "env-remote:proj-a";
    const keyB = "env-local:proj-b";
    const keyC = "env-local:proj-c";
    const currentOrder = [keyALocal, keyARemote, keyB, keyC];

    const next = reorderProjects(makeUiState(), currentOrder, [keyALocal, keyARemote], [keyC]);

    expect(next.projectOrder).toEqual([keyB, keyC, keyALocal, keyARemote]);
  });

  it("does not reorder missing or identical groups", () => {
    const currentOrder = ["env-local:proj-a", "env-local:proj-b"];
    const state = makeUiState();

    expect(reorderProjects(state, currentOrder, ["env-local:missing"], ["env-local:proj-b"])).toBe(
      state,
    );
    expect(reorderProjects(state, currentOrder, ["env-local:proj-a"], ["env-local:proj-a"])).toBe(
      state,
    );
    expect(reorderProjects(state, currentOrder, [], ["env-local:proj-b"])).toBe(state);
  });

  it("stores only collapsed changed-file turns", () => {
    const threadId = ThreadId.make("thread-1");
    const collapsed = setThreadChangedFilesExpanded(makeUiState(), threadId, "turn-1", false);

    expect(collapsed.threadChangedFilesExpandedById).toEqual({
      [threadId]: {
        "turn-1": false,
      },
    });
    expect(
      setThreadChangedFilesExpanded(collapsed, threadId, "turn-1", true)
        .threadChangedFilesExpandedById,
    ).toEqual({});

    const twoTurns = setThreadChangedFilesExpanded(collapsed, threadId, "turn-2", false);
    expect(
      setThreadChangedFilesExpanded(twoTurns, threadId, "turn-1", true)
        .threadChangedFilesExpandedById,
    ).toEqual({ [threadId]: { "turn-2": false } });
  });

  it("stores the endpoint preference by stable key", () => {
    const next = setDefaultAdvertisedEndpointKey(makeUiState(), "desktop-core:lan:http");

    expect(next.defaultAdvertisedEndpointKey).toBe("desktop-core:lan:http");
    expect(setDefaultAdvertisedEndpointKey(next, "desktop-core:lan:http")).toBe(next);
    expect(setDefaultAdvertisedEndpointKey(next, "")).toMatchObject({
      defaultAdvertisedEndpointKey: null,
    });
  });
});

describe("parsePersistedState", () => {
  it("hydrates raw UI-owned state without server entities", () => {
    const parsed = parsePersistedState({
      agentsSectionExpanded: false,
      agentsGroupExpandedById: {
        done: true,
        invalid: "no" as unknown as boolean,
      },
      projectExpandedById: {
        logical: false,
        invalid: "no" as unknown as boolean,
      },
      projectOrder: ["physical-b", "", "physical-a", "physical-b"],
      threadLastVisitedAtById: {
        "environment:thread-1": "2026-02-25T12:35:00.000Z",
        invalid: "not-a-date",
      },
      defaultAdvertisedEndpointKey: "desktop-core:lan:http",
      threadChangedFilesExpandedById: {
        "environment:thread-1": {
          "turn-1": false,
          "turn-2": true,
        },
      },
    });

    expect(parsed).toEqual({
      agentsSectionExpanded: false,
      sidebarView: "environments",
      repositoryGroupExpandedById: {},
      agentsGroupExpandedById: {
        done: true,
      },
      projectExpandedById: {
        logical: false,
      },
      projectOrder: ["physical-b", "physical-a"],
      threadLastVisitedAtById: {
        "environment:thread-1": "2026-02-25T12:35:00.000Z",
      },
      defaultAdvertisedEndpointKey: "desktop-core:lan:http",
      threadChangedFilesExpandedById: {
        "environment:thread-1": {
          "turn-1": false,
        },
      },
    });
  });

  it("migrates legacy CWD project preferences into local alias keys", () => {
    const parsed = parsePersistedState({
      collapsedProjectCwds: ["/repo/b"],
      expandedProjectCwds: ["/repo/a"],
      projectOrderCwds: ["/repo/b", "/repo/a"],
    });
    const projectAKey = legacyProjectCwdPreferenceKey("/repo/a");
    const projectBKey = legacyProjectCwdPreferenceKey("/repo/b");

    expect(parsed.projectOrder).toEqual([projectBKey, projectAKey]);
    expect(resolveProjectExpanded(parsed.projectExpandedById, [projectAKey])).toBe(true);
    expect(resolveProjectExpanded(parsed.projectExpandedById, [projectBKey])).toBe(false);
    expect(resolveProjectExpanded(parsed.projectExpandedById, ["unknown"])).toBe(true);
    expect(parsed.agentsSectionExpanded).toBe(true);
    expect(parsed.agentsGroupExpandedById).toEqual({});
  });

  it("preserves legacy expanded-only semantics for one-way migration", () => {
    const parsed = parsePersistedState({
      expandedProjectCwds: ["/repo/a"],
    });

    expect(
      resolveProjectExpanded(parsed.projectExpandedById, [
        legacyProjectCwdPreferenceKey("/repo/a"),
      ]),
    ).toBe(true);
    expect(
      resolveProjectExpanded(parsed.projectExpandedById, [
        legacyProjectCwdPreferenceKey("/repo/b"),
      ]),
    ).toBe(false);
  });

  it("sanitizes malformed records and changed-file collections", () => {
    const parsed = parsePersistedState({
      agentsSectionExpanded: "no" as never,
      agentsGroupExpandedById: null as never,
      projectExpandedById: null as never,
      projectOrder: null as never,
      threadLastVisitedAtById: null as never,
      threadChangedFilesExpandedById: {
        "": { turn: false },
        thread: null as never,
        empty: { expanded: true, invalid: "no" as never, "": false },
      },
      defaultAdvertisedEndpointKey: "",
    });

    expect(parsed).toEqual(makeUiState());
  });
});

function createLocalStorageStub(): Storage {
  const store = new Map<string, string>();
  return {
    clear: () => {
      store.clear();
    },
    getItem: (key) => store.get(key) ?? null,
    key: (index) => [...store.keys()][index] ?? null,
    get length() {
      return store.size;
    },
    removeItem: (key) => {
      store.delete(key);
    },
    setItem: (key, value) => {
      store.set(key, value);
    },
  };
}

describe("uiStateStore persistence", () => {
  let localStorageStub: Storage;

  beforeEach(() => {
    localStorageStub = createLocalStorageStub();
    vi.stubGlobal("window", { localStorage: localStorageStub });
    vi.stubGlobal("localStorage", localStorageStub);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("persists raw UI preferences including thread visit markers", () => {
    const state = makeUiState({
      agentsSectionExpanded: false,
      agentsGroupExpandedById: {
        done: true,
      },
      sidebarView: "repositories",
      repositoryGroupExpandedById: { "github.com/acme/repo": false },
      projectExpandedById: {
        logical: false,
      },
      projectOrder: ["physical-b", "physical-a"],
      threadLastVisitedAtById: {
        "environment:thread-1": "2026-02-25T12:35:00.000Z",
      },
      threadChangedFilesExpandedById: {
        "environment:thread-1": {
          "turn-1": false,
          "turn-2": true,
        },
      },
      defaultAdvertisedEndpointKey: "desktop-core:lan:http",
    });

    persistState(state);

    const persisted = JSON.parse(
      localStorageStub.getItem(PERSISTED_STATE_KEY) ?? "{}",
    ) as PersistedUiState;
    expect(persisted).toEqual({
      agentsSectionExpanded: false,
      sidebarView: "repositories",
      repositoryGroupExpandedById: { "github.com/acme/repo": false },
      agentsGroupExpandedById: {
        done: true,
      },
      projectExpandedById: {
        logical: false,
      },
      projectOrder: ["physical-b", "physical-a"],
      threadLastVisitedAtById: {
        "environment:thread-1": "2026-02-25T12:35:00.000Z",
      },
      defaultAdvertisedEndpointKey: "desktop-core:lan:http",
      threadChangedFilesExpandedById: {
        "environment:thread-1": {
          "turn-1": false,
        },
      },
    });
    expect(parsePersistedState(persisted)).toEqual({
      ...state,
      threadChangedFilesExpandedById: {
        "environment:thread-1": {
          "turn-1": false,
        },
      },
    });
  });

  it("drops the temporary expanded-only migration fallback when rewriting state", () => {
    const migrated = parsePersistedState({
      expandedProjectCwds: ["/repo/a"],
    });

    persistState(migrated);

    const persisted = JSON.parse(
      localStorageStub.getItem(PERSISTED_STATE_KEY) ?? "{}",
    ) as PersistedUiState;
    expect(resolveProjectExpanded(persisted.projectExpandedById ?? {}, ["unknown"])).toBe(true);
  });

  it("omits fully expanded changed-file maps and tolerates missing browser storage", () => {
    persistState(
      makeUiState({
        threadChangedFilesExpandedById: { thread: { turn: true } },
      }),
    );
    const persisted = JSON.parse(
      localStorageStub.getItem(PERSISTED_STATE_KEY) ?? "{}",
    ) as PersistedUiState;
    expect(persisted.threadChangedFilesExpandedById).toEqual({});

    vi.unstubAllGlobals();
    expect(() => persistState(makeUiState())).not.toThrow();
  });
});
