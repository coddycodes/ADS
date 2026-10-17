import {
  CheckpointRef,
  EnvironmentId,
  EventId,
  MessageId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  TurnId,
  type OrchestrationShellSnapshot,
  type OrchestrationThread,
} from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Option from "effect/Option";
import { AsyncResult, Atom, AtomRegistry } from "effect/unstable/reactivity";

import { PrimaryConnectionTarget } from "../connection/model.ts";
import {
  InvalidScopedProjectKeyError,
  InvalidScopedProjectRefCollectionKeyError,
  InvalidScopedThreadKeyError,
  parseProjectKey,
  parseProjectRefCollectionKey,
  parseThreadKey,
} from "./entities.ts";
import type { EnvironmentShellState } from "./shell.ts";
import { EMPTY_ENVIRONMENT_THREAD_STATE, type EnvironmentThreadState } from "./threads.ts";
import { createEnvironmentProjectAtoms } from "./projectEntities.ts";
import { createEnvironmentSnapshotAtom } from "./snapshots.ts";
import { createEnvironmentThreadDetailAtoms } from "./threadDetail.ts";
import { mergeEnvironmentThread } from "./threadDetail.ts";
import { createEnvironmentThreadShellAtoms } from "./threadShell.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const PROJECT_ID = ProjectId.make("project-1");
const OTHER_PROJECT_ID = ProjectId.make("project-2");
const THREAD_ID = ThreadId.make("thread-1");
const OTHER_THREAD_ID = ThreadId.make("thread-2");

describe("scoped entity keys", () => {
  it("preserves an invalid project key as structured error data", () => {
    const key = "missing-project-key-separator";
    let error: unknown;

    try {
      parseProjectKey(key);
    } catch (cause) {
      error = cause;
    }

    expect(error).toEqual(new InvalidScopedProjectKeyError({ key }));
  });

  it("preserves an invalid thread key as structured error data", () => {
    const key = "missing-thread-key-separator";
    let error: unknown;

    try {
      parseThreadKey(key);
    } catch (cause) {
      error = cause;
    }

    expect(error).toEqual(new InvalidScopedThreadKeyError({ key }));
  });

  it("preserves malformed project reference collection input and its cause", () => {
    const key = "not-json";
    let error: unknown;

    try {
      parseProjectRefCollectionKey(key);
    } catch (cause) {
      error = cause;
    }

    expect(error).toBeInstanceOf(InvalidScopedProjectRefCollectionKeyError);
    expect(error).toMatchObject({ key, cause: expect.anything() });
  });

  it("rejects invalid project reference collection shapes", () => {
    const key = JSON.stringify([["environment-1"]]);

    expect(() => parseProjectRefCollectionKey(key)).toThrowError(
      InvalidScopedProjectRefCollectionKeyError,
    );
  });
});

const THREAD_SHELL = {
  id: THREAD_ID,
  projectId: PROJECT_ID,
  title: "Thread",
  modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
  runtimeMode: "full-access",
  interactionMode: "default",
  branch: null,
  worktreePath: null,
  latestTurn: null,
  createdAt: "2026-06-01T00:00:00.000Z",
  updatedAt: "2026-06-01T00:00:00.000Z",
  archivedAt: null,
  session: null,
  latestUserMessageAt: null,
  hasPendingApprovals: false,
  hasPendingUserInput: false,
  hasActionableProposedPlan: false,
} as const;

const SNAPSHOT: OrchestrationShellSnapshot = {
  snapshotSequence: 1,
  updatedAt: "2026-06-01T00:00:00.000Z",
  projects: [
    {
      id: PROJECT_ID,
      title: "Project",
      workspaceRoot: "/repo",
      repositoryIdentity: null,
      defaultModelSelection: null,
      scripts: [],
      worktreeDiscovery: {
        visibility: "hidden",
        initialPromptDismissedAt: null,
        baselinePaths: [],
      },
      createdAt: "2026-06-01T00:00:00.000Z",
      updatedAt: "2026-06-01T00:00:00.000Z",
    },
    {
      id: OTHER_PROJECT_ID,
      title: "Other project",
      workspaceRoot: "/other-repo",
      repositoryIdentity: null,
      defaultModelSelection: null,
      scripts: [],
      worktreeDiscovery: {
        visibility: "hidden",
        initialPromptDismissedAt: null,
        baselinePaths: [],
      },
      createdAt: "2026-06-01T00:00:00.000Z",
      updatedAt: "2026-06-01T00:00:00.000Z",
    },
  ],
  threads: [
    THREAD_SHELL,
    {
      ...THREAD_SHELL,
      id: OTHER_THREAD_ID,
      projectId: OTHER_PROJECT_ID,
      title: "Other thread",
    },
  ],
};

function shellState(snapshot: OrchestrationShellSnapshot): EnvironmentShellState {
  return {
    snapshot: Option.some(snapshot),
    status: "live",
    error: Option.none(),
  };
}

function makeHarness() {
  const shellStateAtoms = Atom.family((_environmentId: EnvironmentId) =>
    Atom.make(AsyncResult.success(shellState(SNAPSHOT))),
  );
  const threadStateAtoms = Atom.family((_key: string) =>
    Atom.make(AsyncResult.success(EMPTY_ENVIRONMENT_THREAD_STATE)),
  );
  const catalogValueAtom = Atom.make({
    isReady: true,
    entries: new Map([
      [
        ENVIRONMENT_ID,
        {
          target: new PrimaryConnectionTarget({
            environmentId: ENVIRONMENT_ID,
            label: "Environment",
            httpBaseUrl: "https://example.test",
            wsBaseUrl: "wss://example.test",
          }),
          profile: Option.none(),
        },
      ],
    ]),
  });
  const snapshotAtom = createEnvironmentSnapshotAtom(shellStateAtoms);
  const projects = createEnvironmentProjectAtoms({
    catalogValueAtom,
    snapshotAtom,
  });
  const threadShells = createEnvironmentThreadShellAtoms({
    catalogValueAtom,
    snapshotAtom,
  });
  const threadDetails = createEnvironmentThreadDetailAtoms((environmentId, threadId) =>
    threadStateAtoms(`${environmentId}\u0000${threadId}`),
  );

  return {
    registry: AtomRegistry.make(),
    shellStateAtom: shellStateAtoms(ENVIRONMENT_ID),
    threadStateAtom: (threadId: ThreadId) => threadStateAtoms(`${ENVIRONMENT_ID}\u0000${threadId}`),
    projects,
    threadShells,
    threadDetails,
  };
}

describe("environment entity projections", () => {
  it("projects empty thread collections when the shell snapshot is unavailable", () => {
    const harness = makeHarness();
    const threadAtom = harness.threadShells.threadShellAtom({
      environmentId: ENVIRONMENT_ID,
      threadId: THREAD_ID,
    });
    expect(harness.registry.get(threadAtom)).toMatchObject({ id: THREAD_ID });
    expect(harness.registry.get(harness.threadShells.threadRefsAtom)).toHaveLength(2);
    expect(harness.registry.get(harness.threadShells.threadShellsAtom)).toHaveLength(2);

    harness.registry.set(
      harness.shellStateAtom,
      AsyncResult.success<EnvironmentShellState>({
        snapshot: Option.none(),
        status: "unavailable",
        error: Option.none(),
      }),
    );

    expect(
      harness.registry.get(harness.threadShells.environmentThreadsAtom(ENVIRONMENT_ID)),
    ).toEqual([]);
    expect(
      harness.registry.get(harness.threadShells.environmentThreadIndexAtom(ENVIRONMENT_ID)).size,
    ).toBe(0);
    expect(
      harness.registry.get(harness.threadShells.environmentThreadRefsByProjectAtom(ENVIRONMENT_ID))
        .size,
    ).toBe(0);
    expect(harness.registry.get(threadAtom)).toBeNull();
    expect(harness.registry.get(harness.threadShells.threadRefsAtom)).toEqual([]);
    expect(harness.registry.get(harness.threadShells.threadShellsAtom)).toEqual([]);
    expect(
      harness.registry.get(
        harness.threadShells.threadShellsForProjectRefsAtom([
          { environmentId: ENVIRONMENT_ID, projectId: PROJECT_ID },
        ]),
      ),
    ).toEqual([]);
  });

  it("groups sibling threads and deduplicates repeated project references", () => {
    const harness = makeHarness();
    harness.registry.set(
      harness.shellStateAtom,
      AsyncResult.success(
        shellState({
          ...SNAPSHOT,
          threads: SNAPSHOT.threads.map((thread) => ({ ...thread, projectId: PROJECT_ID })),
        }),
      ),
    );
    const projectRef = { environmentId: ENVIRONMENT_ID, projectId: PROJECT_ID };
    const missingProjectRef = {
      environmentId: ENVIRONMENT_ID,
      projectId: ProjectId.make("missing-project"),
    };

    expect(
      harness.registry
        .get(harness.threadShells.environmentThreadRefsByProjectAtom(ENVIRONMENT_ID))
        .get(PROJECT_ID),
    ).toHaveLength(2);
    expect(
      harness.registry.get(
        harness.threadShells.threadShellsForProjectRefsAtom([
          projectRef,
          missingProjectRef,
          projectRef,
        ]),
      ),
    ).toHaveLength(2);
  });

  it("composes detail collections with authoritative shell workspace metadata", () => {
    const messages: OrchestrationThread["messages"] = [];
    const detail = {
      ...THREAD_SHELL,
      environmentId: ENVIRONMENT_ID,
      title: "Cached thread",
      branch: "stale-branch",
      worktreePath: "/repo/stale-worktree",
      deletedAt: null,
      messages,
      proposedPlans: [],
      activities: [],
      checkpoints: [],
    } satisfies OrchestrationThread & { readonly environmentId: EnvironmentId };
    const shell = {
      ...THREAD_SHELL,
      environmentId: ENVIRONMENT_ID,
      title: "Current thread",
      branch: "current-branch",
      worktreePath: "/repo/current-worktree",
    };

    const merged = mergeEnvironmentThread(detail, shell);

    expect(merged).toMatchObject({
      title: "Current thread",
      branch: "current-branch",
      worktreePath: "/repo/current-worktree",
    });
    expect(merged?.messages).toBe(messages);
  });

  it("leaves incomplete or mismatched detail and shell pairs unchanged", () => {
    const detail = {
      ...THREAD_SHELL,
      environmentId: ENVIRONMENT_ID,
      deletedAt: null,
      messages: [],
      proposedPlans: [],
      activities: [],
      checkpoints: [],
    } satisfies OrchestrationThread & { readonly environmentId: EnvironmentId };
    const shell = { ...THREAD_SHELL, environmentId: ENVIRONMENT_ID };

    expect(mergeEnvironmentThread(null, shell)).toBeNull();
    expect(mergeEnvironmentThread(detail, null)).toBe(detail);
    expect(
      mergeEnvironmentThread(detail, { ...shell, environmentId: EnvironmentId.make("other") }),
    ).toBe(detail);
    expect(mergeEnvironmentThread(detail, { ...shell, id: ThreadId.make("other-thread") })).toBe(
      detail,
    );
  });

  it("preserves untouched project and thread identities across unrelated shell updates", () => {
    const harness = makeHarness();
    const projectRefsAtom = harness.projects.environmentProjectRefsAtom(ENVIRONMENT_ID);
    const threadRefsAtom = harness.threadShells.environmentThreadRefsAtom(ENVIRONMENT_ID);
    const projectsAtom = harness.projects.projectsAtom;
    const projectAtom = harness.projects.projectAtom({
      environmentId: ENVIRONMENT_ID,
      projectId: PROJECT_ID,
    });
    const threadAtom = harness.threadShells.threadShellAtom({
      environmentId: ENVIRONMENT_ID,
      threadId: THREAD_ID,
    });
    const projectRefs = harness.registry.get(projectRefsAtom);
    const threadRefs = harness.registry.get(threadRefsAtom);
    const allThreadRefs = harness.registry.get(harness.threadShells.threadRefsAtom);
    const allThreadShells = harness.registry.get(harness.threadShells.threadShellsAtom);
    const projects = harness.registry.get(projectsAtom);
    const project = harness.registry.get(projectAtom);
    const thread = harness.registry.get(threadAtom);

    harness.registry.set(
      harness.shellStateAtom,
      AsyncResult.success(
        shellState({
          ...SNAPSHOT,
          snapshotSequence: 2,
          projects: SNAPSHOT.projects.map((candidate) =>
            candidate.id === OTHER_PROJECT_ID
              ? { ...candidate, title: "Renamed other project" }
              : candidate,
          ),
        }),
      ),
    );
    expect(harness.registry.get(harness.threadShells.threadRefsAtom)).toBe(allThreadRefs);
    expect(harness.registry.get(harness.threadShells.threadShellsAtom)).toBe(allThreadShells);

    harness.registry.set(
      harness.shellStateAtom,
      AsyncResult.success(
        shellState({
          ...SNAPSHOT,
          snapshotSequence: 3,
          threads: SNAPSHOT.threads.map((candidate) =>
            candidate.id === OTHER_THREAD_ID
              ? { ...candidate, title: "Renamed other thread" }
              : candidate,
          ),
        }),
      ),
    );

    expect(harness.registry.get(projectRefsAtom)).toBe(projectRefs);
    expect(harness.registry.get(threadRefsAtom)).toBe(threadRefs);
    expect(harness.registry.get(harness.threadShells.threadRefsAtom)).toBe(allThreadRefs);
    expect(harness.registry.get(projectsAtom)).toBe(projects);
    expect(harness.registry.get(projectAtom)).toBe(project);
    expect(harness.registry.get(threadAtom)).toBe(thread);
  });

  it("preserves project-scoped thread collections across unrelated project updates", () => {
    const harness = makeHarness();
    const projectRef = {
      environmentId: ENVIRONMENT_ID,
      projectId: PROJECT_ID,
    };
    const refsByProjectAtom =
      harness.threadShells.environmentThreadRefsByProjectAtom(ENVIRONMENT_ID);
    const threadsAtom = harness.threadShells.threadShellsForProjectRefsAtom([projectRef]);
    const refs = harness.registry.get(refsByProjectAtom).get(PROJECT_ID);
    const threads = harness.registry.get(threadsAtom);

    expect(threads).toHaveLength(1);
    expect(threads[0]?.id).toBe(THREAD_ID);

    harness.registry.set(
      harness.shellStateAtom,
      AsyncResult.success(
        shellState({
          ...SNAPSHOT,
          snapshotSequence: 2,
          threads: SNAPSHOT.threads.map((thread) =>
            thread.id === OTHER_THREAD_ID ? { ...thread, title: "Updated elsewhere" } : thread,
          ),
        }),
      ),
    );

    expect(harness.registry.get(refsByProjectAtom).get(PROJECT_ID)).toBe(refs);
    expect(harness.registry.get(threadsAtom)).toBe(threads);
  });

  it("updates only the requested thread detail and preserves untouched field identities", () => {
    const harness = makeHarness();
    const threadRef = {
      environmentId: ENVIRONMENT_ID,
      threadId: THREAD_ID,
    };
    const otherThreadRef = {
      environmentId: ENVIRONMENT_ID,
      threadId: OTHER_THREAD_ID,
    };
    const threadDetailAtom = harness.threadDetails.detailAtom(threadRef);
    const messagesAtom = harness.threadDetails.messagesAtom(threadRef);
    const activitiesAtom = harness.threadDetails.activitiesAtom(threadRef);
    const statusAtom = harness.threadDetails.statusAtom(threadRef);
    const otherThreadDetailAtom = harness.threadDetails.detailAtom(otherThreadRef);
    const otherValue = harness.registry.get(otherThreadDetailAtom);
    const detail = {
      ...THREAD_SHELL,
      deletedAt: null,
      messages: [],
      proposedPlans: [],
      activities: [],
      checkpoints: [],
    } satisfies OrchestrationThread;

    harness.registry.set(
      harness.threadStateAtom(THREAD_ID),
      AsyncResult.success<EnvironmentThreadState>({
        data: Option.some(detail),
        status: "live",
        error: Option.none(),
      }),
    );

    const scopedDetail = harness.registry.get(threadDetailAtom);
    const messages = harness.registry.get(messagesAtom);
    const activities = harness.registry.get(activitiesAtom);

    expect(scopedDetail).toEqual({ ...detail, environmentId: ENVIRONMENT_ID });
    expect(harness.registry.get(statusAtom)).toBe("live");
    expect(harness.registry.get(otherThreadDetailAtom)).toBe(otherValue);

    harness.registry.set(
      harness.threadStateAtom(THREAD_ID),
      AsyncResult.success<EnvironmentThreadState>({
        data: Option.some({
          ...detail,
          session: {
            threadId: THREAD_ID,
            status: "ready",
            providerName: "codex",
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: null,
            updatedAt: "2026-06-01T00:01:00.000Z",
          },
        }),
        status: "live",
        error: Option.none(),
      }),
    );

    expect(harness.registry.get(messagesAtom)).toBe(messages);
    expect(harness.registry.get(activitiesAtom)).toBe(activities);
  });

  // A reconnect resends the whole thread as a new object graph. Handing that to the UI as a new
  // thread re-renders it, and memoized closures from earlier renders keep each superseded copy
  // alive (#96), so unchanged parts keep the identity they already had.
  it("keeps the identity of everything a resent thread did not change", () => {
    const harness = makeHarness();
    const ref = { environmentId: ENVIRONMENT_ID, threadId: THREAD_ID };
    const detailAtom = harness.threadDetails.detailAtom(ref);
    const messagesAtom = harness.threadDetails.messagesAtom(ref);
    const snapshot = (texts: ReadonlyArray<string>): OrchestrationThread => ({
      ...THREAD_SHELL,
      deletedAt: null,
      messages: texts.map((text, index) => ({
        id: MessageId.make(`message-${index}`),
        role: "assistant",
        text,
        turnId: null,
        streaming: false,
        createdAt: "2026-06-01T00:00:00.000Z",
        updatedAt: "2026-06-01T00:00:00.000Z",
      })),
      proposedPlans: [],
      activities: [],
      checkpoints: [],
    });
    const receive = (thread: OrchestrationThread) =>
      harness.registry.set(
        harness.threadStateAtom(THREAD_ID),
        AsyncResult.success<EnvironmentThreadState>({
          data: Option.some(thread),
          status: "live",
          error: Option.none(),
        }),
      );

    receive(snapshot(["first", "second"]));
    const detail = harness.registry.get(detailAtom);
    const messages = harness.registry.get(messagesAtom);

    receive(snapshot(["first", "second"]));
    expect(harness.registry.get(detailAtom)).toBe(detail);
    expect(harness.registry.get(messagesAtom)).toBe(messages);

    receive(snapshot(["first", "edited"]));
    const edited = harness.registry.get(messagesAtom);
    expect(edited).not.toBe(messages);
    expect(edited[0]).toBe(messages[0]);
    expect(edited[1]).toMatchObject({ text: "edited" });
    expect(harness.registry.get(detailAtom)).toMatchObject({ messages: edited });
  });

  // Patches share every object they did not change with the source they patched. After a resend
  // those objects are not the ones the UI holds, yet a streaming patch must still not re-compare
  // the whole history on every token.
  it("does not re-compare history a patch left untouched after a resend", () => {
    const harness = makeHarness();
    const ref = { environmentId: ENVIRONMENT_ID, threadId: THREAD_ID };
    const messagesAtom = harness.threadDetails.messagesAtom(ref);
    let historyReads = 0;
    const message = (index: number, text: string, counted = false) => {
      const fields = {
        id: MessageId.make(`message-${index}`),
        role: "assistant" as const,
        turnId: null,
        streaming: false,
        createdAt: "2026-06-01T00:00:00.000Z",
        updatedAt: "2026-06-01T00:00:00.000Z",
      };
      return counted
        ? Object.defineProperty(
            { ...fields } as typeof fields & { readonly text: string },
            "text",
            {
              enumerable: true,
              get: () => {
                historyReads += 1;
                return text;
              },
            },
          )
        : { ...fields, text };
    };
    const thread = (messages: OrchestrationThread["messages"]): OrchestrationThread => ({
      ...THREAD_SHELL,
      deletedAt: null,
      messages,
      proposedPlans: [],
      activities: [],
      checkpoints: [],
    });
    const receive = (value: OrchestrationThread) => {
      harness.registry.set(
        harness.threadStateAtom(THREAD_ID),
        AsyncResult.success<EnvironmentThreadState>({
          data: Option.some(value),
          status: "live",
          error: Option.none(),
        }),
      );
      return harness.registry.get(messagesAtom);
    };

    const shown = receive(thread([message(0, "history"), message(1, "reply")]));
    const resent = thread([message(0, "history", true), message(1, "reply")]);
    expect(receive(resent)).toBe(shown);
    historyReads = 0;

    const patched = receive(thread([resent.messages[0]!, message(1, "reply, streaming")]));

    expect(patched[0]).toBe(shown[0]);
    expect(patched[1]).toMatchObject({ text: "reply, streaming" });
    expect(historyReads).toBe(0);
  });

  it("keeps a changed payload's own __proto__ key as data", () => {
    const harness = makeHarness();
    const ref = { environmentId: ENVIRONMENT_ID, threadId: THREAD_ID };
    const activitiesAtom = harness.threadDetails.activitiesAtom(ref);
    const receive = (payloadJson: string) =>
      harness.registry.set(
        harness.threadStateAtom(THREAD_ID),
        AsyncResult.success<EnvironmentThreadState>({
          data: Option.some({
            ...THREAD_SHELL,
            deletedAt: null,
            messages: [],
            proposedPlans: [],
            activities: [
              {
                id: EventId.make("activity-1"),
                tone: "tool",
                kind: "tool.call",
                summary: "Tool call",
                payload: JSON.parse(payloadJson),
                turnId: null,
                createdAt: "2026-06-01T00:00:00.000Z",
              },
            ],
            checkpoints: [],
          }),
          status: "live",
          error: Option.none(),
        }),
      );

    receive('{"__proto__":{"tool":"old"},"value":1}');
    harness.registry.get(activitiesAtom);
    receive('{"__proto__":{"tool":"new"},"value":2}');

    const payload = harness.registry.get(activitiesAtom)[0]?.payload as object;
    expect(Object.getPrototypeOf(payload)).toBe(Object.prototype);
    expect(Object.getOwnPropertyDescriptor(payload, "__proto__")?.value).toEqual({ tool: "new" });
    expect(payload).toMatchObject({ value: 2 });
  });

  it("projects every detail collection, status, error, session, and latest turn", () => {
    const harness = makeHarness();
    const ref = { environmentId: ENVIRONMENT_ID, threadId: THREAD_ID };
    const atoms = harness.threadDetails;
    const stateAtom = atoms.stateAtom(ref);
    const detailAtom = atoms.detailAtom(ref);
    const statusAtom = atoms.statusAtom(ref);
    const errorAtom = atoms.errorAtom(ref);
    const messagesAtom = atoms.messagesAtom(ref);
    const activitiesAtom = atoms.activitiesAtom(ref);
    const plansAtom = atoms.proposedPlansAtom(ref);
    const checkpointsAtom = atoms.checkpointsAtom(ref);
    const sessionAtom = atoms.sessionAtom(ref);
    const latestTurnAtom = atoms.latestTurnAtom(ref);

    expect(harness.registry.get(stateAtom)).toEqual(EMPTY_ENVIRONMENT_THREAD_STATE);
    expect(harness.registry.get(detailAtom)).toBeNull();
    expect(harness.registry.get(statusAtom)).toBe("empty");
    expect(harness.registry.get(errorAtom)).toBeNull();
    expect(harness.registry.get(messagesAtom)).toEqual([]);
    expect(harness.registry.get(activitiesAtom)).toEqual([]);
    expect(harness.registry.get(plansAtom)).toEqual([]);
    expect(harness.registry.get(checkpointsAtom)).toEqual([]);
    expect(harness.registry.get(sessionAtom)).toBeNull();
    expect(harness.registry.get(latestTurnAtom)).toBeNull();

    const message = {
      id: MessageId.make("message-projected"),
      role: "assistant" as const,
      text: "done",
      turnId: TurnId.make("turn-projected"),
      streaming: false,
      createdAt: "created",
      updatedAt: "updated",
    };
    const activity = {
      id: EventId.make("activity-projected"),
      tone: "tool" as const,
      kind: "command" as const,
      summary: "ran",
      payload: {},
      turnId: TurnId.make("turn-projected"),
      createdAt: "created",
    };
    const plan = {
      id: "plan-projected",
      turnId: TurnId.make("turn-projected"),
      planMarkdown: "plan",
      implementedAt: null,
      implementationThreadId: null,
      createdAt: "created",
      updatedAt: "updated",
    };
    const checkpoint = {
      turnId: TurnId.make("turn-projected"),
      checkpointTurnCount: 1,
      checkpointRef: CheckpointRef.make("checkpoint-projected"),
      status: "ready" as const,
      files: [],
      assistantMessageId: message.id,
      completedAt: "completed",
    };
    const latestTurn = {
      turnId: TurnId.make("turn-projected"),
      state: "completed" as const,
      requestedAt: "requested",
      startedAt: "started",
      completedAt: "completed",
      assistantMessageId: message.id,
    };
    const session = {
      threadId: THREAD_ID,
      status: "ready" as const,
      providerName: "codex",
      runtimeMode: "full-access" as const,
      activeTurnId: null,
      lastError: null,
      updatedAt: "updated",
    };
    const thread = {
      ...THREAD_SHELL,
      deletedAt: null,
      messages: [message],
      activities: [activity],
      proposedPlans: [plan],
      checkpoints: [checkpoint],
      latestTurn,
      session,
    } satisfies OrchestrationThread;
    harness.registry.set(
      harness.threadStateAtom(THREAD_ID),
      AsyncResult.success<EnvironmentThreadState>({
        data: Option.some(thread),
        status: "live",
        error: Option.some("warning"),
      }),
    );

    expect(harness.registry.get(detailAtom)).toMatchObject({ environmentId: ENVIRONMENT_ID });
    expect(harness.registry.get(statusAtom)).toBe("live");
    expect(harness.registry.get(errorAtom)).toBe("warning");
    expect(harness.registry.get(messagesAtom)).toBe(thread.messages);
    expect(harness.registry.get(activitiesAtom)).toBe(thread.activities);
    expect(harness.registry.get(plansAtom)).toBe(thread.proposedPlans);
    expect(harness.registry.get(checkpointsAtom)).toBe(thread.checkpoints);
    expect(harness.registry.get(sessionAtom)).toBe(session);
    expect(harness.registry.get(latestTurnAtom)).toBe(latestTurn);
    expect(harness.registry.get(detailAtom)).toBe(harness.registry.get(detailAtom));
    harness.registry.dispose();
  });
});
