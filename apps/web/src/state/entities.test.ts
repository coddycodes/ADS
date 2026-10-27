import { EnvironmentId, ProjectId, ThreadId } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => {
  const markers = new Map<string, { name: string }>();
  const marker = (name: string) => {
    const existing = markers.get(name);
    if (existing) return existing;
    const value = { name };
    markers.set(name, value);
    return value;
  };
  return {
    marker,
    atomValues: new Map<unknown, unknown>(),
    useAtomValue: vi.fn((atom: unknown) => harness.atomValues.get(atom)),
    registryGet: vi.fn(),
    registrySet: vi.fn(),
    mergeThread: vi.fn(),
    projectRefsAtom: marker("projectRefs"),
    projectsAtom: marker("projects"),
    threadRefsAtom: marker("threadRefs"),
    threadShellsAtom: marker("threadShells"),
    serverConfigsAtom: marker("serverConfigs"),
    environmentProjectRefsAtom: vi.fn((id: string) => marker(`projectRefs:${id}`)),
    projectAtom: vi.fn((ref: { projectId: string }) => marker(`project:${ref.projectId}`)),
    environmentThreadRefsAtom: vi.fn((id: string) => marker(`threadRefs:${id}`)),
    threadShellsForProjectRefsAtom: vi.fn((_refs: unknown) => marker("threadShellsForProjects")),
    threadShellAtom: vi.fn((ref: { threadId: string }) => marker(`shell:${ref.threadId}`)),
    detailAtom: vi.fn((ref: { threadId: string }) => marker(`detail:${ref.threadId}`)),
    messagesAtom: vi.fn((ref: { threadId: string }) => marker(`messages:${ref.threadId}`)),
    activitiesAtom: vi.fn((ref: { threadId: string }) => marker(`activities:${ref.threadId}`)),
    proposedPlansAtom: vi.fn((ref: { threadId: string }) => marker(`plans:${ref.threadId}`)),
    sessionAtom: vi.fn((ref: { threadId: string }) => marker(`session:${ref.threadId}`)),
  };
});

vi.mock("@effect/atom-react", () => ({ useAtomValue: harness.useAtomValue }));
vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useMemo: (factory: () => unknown) => factory(),
}));
vi.mock("../rpc/atomRegistry", () => ({
  appAtomRegistry: { get: harness.registryGet, set: harness.registrySet },
}));
vi.mock("./projects", () => ({
  environmentProjects: {
    projectRefsAtom: harness.projectRefsAtom,
    projectsAtom: harness.projectsAtom,
    environmentProjectRefsAtom: harness.environmentProjectRefsAtom,
    projectAtom: harness.projectAtom,
  },
}));
vi.mock("./server", () => ({
  environmentServerConfigsAtom: harness.serverConfigsAtom,
}));
vi.mock("./threads", () => ({
  environmentThreadShells: {
    threadRefsAtom: harness.threadRefsAtom,
    threadShellsAtom: harness.threadShellsAtom,
    environmentThreadRefsAtom: harness.environmentThreadRefsAtom,
    threadShellsForProjectRefsAtom: harness.threadShellsForProjectRefsAtom,
    threadShellAtom: harness.threadShellAtom,
  },
  environmentThreadDetails: {
    detailAtom: harness.detailAtom,
    messagesAtom: harness.messagesAtom,
    activitiesAtom: harness.activitiesAtom,
    proposedPlansAtom: harness.proposedPlansAtom,
    sessionAtom: harness.sessionAtom,
  },
}));
vi.mock("@coddy/client-runtime/state/threads", () => ({
  mergeEnvironmentThread: harness.mergeThread,
}));

import {
  activeEnvironmentIdAtom,
  findThreadRef,
  readActiveEnvironmentId,
  readEnvironmentThreadRefs,
  readProject,
  readThreadDetail,
  readThreadRefs,
  readThreadShell,
  setActiveEnvironmentId,
  reconcileEnvironmentSelection,
  useActiveEnvironmentId,
  useEnvironmentProjectRefs,
  useEnvironmentThreadRefs,
  useProject,
  useProjectRefs,
  useProjects,
  useServerConfigs,
  useThread,
  useThreadActivities,
  useThreadDetail,
  useThreadMessages,
  useThreadProposedPlans,
  useThreadRefs,
  useThreadSession,
  useThreadShell,
  useThreadShells,
  useThreadShellsForProjectRefs,
} from "./entities";

const environmentId = EnvironmentId.make("env-1");
const projectRef = { environmentId, projectId: ProjectId.make("project-1") };
const threadRef = { environmentId, threadId: ThreadId.make("thread-1") };

beforeEach(() => {
  harness.atomValues.clear();
  harness.useAtomValue.mockClear();
  harness.registryGet.mockReset();
  harness.registrySet.mockReset();
  harness.mergeThread.mockReset();
  for (const mock of [
    harness.environmentProjectRefsAtom,
    harness.projectAtom,
    harness.environmentThreadRefsAtom,
    harness.threadShellsForProjectRefsAtom,
    harness.threadShellAtom,
    harness.detailAtom,
    harness.messagesAtom,
    harness.activitiesAtom,
    harness.proposedPlansAtom,
    harness.sessionAtom,
  ]) {
    mock.mockClear();
  }
});

describe("web entity selectors", () => {
  it("reads and writes the active environment", () => {
    harness.atomValues.set(activeEnvironmentIdAtom, environmentId);
    harness.registryGet.mockReturnValue(environmentId);

    expect(useActiveEnvironmentId()).toBe(environmentId);
    expect(readActiveEnvironmentId()).toBe(environmentId);
    setActiveEnvironmentId(environmentId);
    setActiveEnvironmentId(null);
    expect(harness.registrySet).toHaveBeenNthCalledWith(1, activeEnvironmentIdAtom, environmentId);
    expect(harness.registrySet).toHaveBeenNthCalledWith(2, activeEnvironmentIdAtom, null);
  });

  it("selects global project, thread, server, and shell collections", () => {
    const projectRefs = [projectRef];
    const projects = [{ id: "project-1" }];
    const threadRefs = [threadRef];
    const shells = [{ id: "thread-1" }];
    const configs = new Map([[environmentId, { version: "1" }]]);
    harness.atomValues.set(harness.projectRefsAtom, projectRefs);
    harness.atomValues.set(harness.projectsAtom, projects);
    harness.atomValues.set(harness.threadRefsAtom, threadRefs);
    harness.atomValues.set(harness.threadShellsAtom, shells);
    harness.atomValues.set(harness.serverConfigsAtom, configs);

    expect(useProjectRefs()).toBe(projectRefs);
    expect(useProjects()).toBe(projects);
    expect(useThreadRefs()).toBe(threadRefs);
    expect(useThreadShells()).toBe(shells);
    expect(useServerConfigs()).toBe(configs);
  });

  it("uses empty atoms for null scoped selectors", () => {
    expect(useEnvironmentProjectRefs(null)).toBeUndefined();
    expect(useEnvironmentThreadRefs(null)).toBeUndefined();
    expect(useProject(null)).toBeUndefined();
    expect(useThreadShell(null)).toBeUndefined();
    expect(useThreadDetail(null)).toBeUndefined();
    expect(useThreadMessages(null)).toBeUndefined();
    expect(useThreadActivities(null)).toBeUndefined();
    expect(useThreadProposedPlans(null)).toBeUndefined();
    expect(useThreadSession(null)).toBeUndefined();
    expect(harness.environmentProjectRefsAtom).not.toHaveBeenCalled();
    expect(harness.environmentThreadRefsAtom).not.toHaveBeenCalled();
  });

  it("selects every scoped entity atom", () => {
    const values = new Map([
      [harness.environmentProjectRefsAtom(environmentId), [projectRef]],
      [harness.environmentThreadRefsAtom(environmentId), [threadRef]],
      [harness.projectAtom(projectRef), { id: "project-1" }],
      [harness.threadShellAtom(threadRef), { id: "shell" }],
      [harness.detailAtom(threadRef), { id: "detail" }],
      [harness.messagesAtom(threadRef), [{ id: "message" }]],
      [harness.activitiesAtom(threadRef), [{ id: "activity" }]],
      [harness.proposedPlansAtom(threadRef), [{ id: "plan" }]],
      [harness.sessionAtom(threadRef), { status: "ready" }],
      [harness.threadShellsForProjectRefsAtom([projectRef]), [{ id: "project-shell" }]],
    ]);
    for (const [atom, value] of values) harness.atomValues.set(atom, value);

    expect(useEnvironmentProjectRefs(environmentId)).toEqual([projectRef]);
    expect(useEnvironmentThreadRefs(environmentId)).toEqual([threadRef]);
    expect(useProject(projectRef)).toEqual({ id: "project-1" });
    expect(useThreadShell(threadRef)).toEqual({ id: "shell" });
    expect(useThreadDetail(threadRef)).toEqual({ id: "detail" });
    expect(useThreadMessages(threadRef)).toEqual([{ id: "message" }]);
    expect(useThreadActivities(threadRef)).toEqual([{ id: "activity" }]);
    expect(useThreadProposedPlans(threadRef)).toEqual([{ id: "plan" }]);
    expect(useThreadSession(threadRef)).toEqual({ status: "ready" });
    expect(useThreadShellsForProjectRefs([projectRef])).toEqual([{ id: "project-shell" }]);
  });

  it("merges thread shell and detail values", () => {
    const shell = { id: "shell" };
    const detail = { id: "detail" };
    harness.atomValues.set(harness.threadShellAtom(threadRef), shell);
    harness.atomValues.set(harness.detailAtom(threadRef), detail);
    harness.mergeThread.mockReturnValue({ id: "merged" });

    expect(useThread(threadRef)).toEqual({ id: "merged" });
    expect(harness.mergeThread).toHaveBeenCalledWith(detail, shell);
    useThread(null);
    expect(harness.mergeThread).toHaveBeenLastCalledWith(undefined, undefined);
  });

  it("provides imperative readers and finds thread refs", () => {
    const projectAtom = harness.projectAtom(projectRef);
    const shellAtom = harness.threadShellAtom(threadRef);
    const detailAtom = harness.detailAtom(threadRef);
    const environmentRefsAtom = harness.environmentThreadRefsAtom(environmentId);
    harness.registryGet.mockImplementation((atom: unknown) => {
      if (atom === projectAtom) return { id: "project" };
      if (atom === shellAtom) return { id: "shell" };
      if (atom === detailAtom) return { id: "detail" };
      if (atom === environmentRefsAtom) return [threadRef];
      if (atom === harness.threadRefsAtom) return [threadRef];
      return null;
    });

    expect(readProject(projectRef)).toEqual({ id: "project" });
    expect(readThreadShell(threadRef)).toEqual({ id: "shell" });
    expect(readThreadDetail(threadRef)).toEqual({ id: "detail" });
    expect(readEnvironmentThreadRefs(environmentId)).toEqual([threadRef]);
    expect(readThreadRefs()).toEqual([threadRef]);
    expect(findThreadRef(threadRef.threadId)).toBe(threadRef);
    expect(findThreadRef(ThreadId.make("missing"))).toBeNull();
  });
});

describe("environment selection", () => {
  beforeEach(() => {
    const values = new Map<unknown, EnvironmentId | null>();
    harness.registryGet.mockImplementation((atom: unknown) => values.get(atom) ?? null);
    harness.registrySet.mockImplementation((atom: unknown, value: EnvironmentId | null) => {
      values.set(atom, value);
    });
  });

  it.each([null, "env-remote"])("initializes only an empty selection (%s)", (selected) => {
    setActiveEnvironmentId(selected === null ? null : EnvironmentId.make(selected));
    reconcileEnvironmentSelection(environmentId, "if-unselected");
    expect(readActiveEnvironmentId()).toBe(selected ?? environmentId);
  });

  it.each([
    { selected: null, previous: null, moves: true },
    { selected: "env-old", previous: "env-old", moves: true },
    { selected: "env-remote", previous: "env-old", moves: false },
    { selected: "env-remote", previous: null, moves: false },
    { selected: "env-1", previous: "env-1", moves: false },
  ])("reconciles primary identity with $selected selected", ({ selected, previous, moves }) => {
    if (previous !== null) {
      reconcileEnvironmentSelection(EnvironmentId.make(previous), "follow-primary-identity");
    }
    setActiveEnvironmentId(selected === null ? null : EnvironmentId.make(selected));
    reconcileEnvironmentSelection(environmentId, "follow-primary-identity");
    expect(readActiveEnvironmentId()).toBe(moves ? environmentId : selected);
  });

  it("keeps primary history through startup defaults and unrelated selections", () => {
    const oldPrimary = EnvironmentId.make("env-old");
    const nextPrimary = EnvironmentId.make("env-next");
    reconcileEnvironmentSelection(oldPrimary, "follow-primary-identity");
    reconcileEnvironmentSelection(EnvironmentId.make("env-saved"), "if-unselected");
    reconcileEnvironmentSelection(environmentId, "follow-primary-identity");
    expect(readActiveEnvironmentId()).toBe(environmentId);

    setActiveEnvironmentId(EnvironmentId.make("env-remote"));
    reconcileEnvironmentSelection(nextPrimary, "follow-primary-identity");
    expect(readActiveEnvironmentId()).toBe("env-remote");
    setActiveEnvironmentId(nextPrimary);
    reconcileEnvironmentSelection(environmentId, "follow-primary-identity");
    expect(readActiveEnvironmentId()).toBe(environmentId);
  });
});
