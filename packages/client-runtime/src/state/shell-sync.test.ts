import {
  EnvironmentId,
  ORCHESTRATION_WS_METHODS,
  ProjectId,
  type OrchestrationShellSnapshot,
  type OrchestrationShellStreamItem,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";

import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionStorageChangedError,
  ConnectionTransientError,
  PrimaryConnectionTarget,
  type PreparedConnection,
} from "../connection/model.ts";
import * as EnvironmentSupervisor from "../connection/supervisor.ts";
import * as Persistence from "../platform/persistence.ts";
import * as RpcSession from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import { type EnvironmentAvailabilityStatus, makeEnvironmentShellState } from "./shell.ts";

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});

const LIVE_SHELL_SNAPSHOT: OrchestrationShellSnapshot = {
  snapshotSequence: 1,
  projects: [],
  threads: [],
  updatedAt: "2026-06-06T00:00:00.000Z",
};

const CACHED_PROJECT = {
  id: ProjectId.make("cached-project"),
  title: "Cached project",
  workspaceRoot: "/cached/project",
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
} as const;

const yieldForShell = Effect.forEach(Array.from({ length: 10 }), () => Effect.yieldNow, {
  discard: true,
});

function session(client: WsRpcProtocolClient): RpcSession.RpcSession {
  return {
    client,
    initialConfig: Effect.never,
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

describe("environment shell synchronization", () => {
  it.effect("ignores all deltas until the current session publishes a full snapshot", () =>
    Effect.gen(function* () {
      const events = yield* Queue.unbounded<OrchestrationShellStreamItem>();
      const client = {
        [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(events),
      } as unknown as WsRpcProtocolClient;
      const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
      const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
        Option.some(session(client)),
      );
      const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
        target: TARGET,
        state: supervisorState,
        session: activeSession,
        prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
        connect: Effect.void,
        disconnect: Effect.void,
        retryNow: Effect.void,
      } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
      const saveCount = yield* Ref.make(0);
      const cache = Persistence.EnvironmentCacheStore.of({
        loadShell: () => Effect.succeed(Option.none()),
        saveShell: () => Ref.update(saveCount, (count) => count + 1),
        loadThread: () => Effect.succeed(Option.none()),
        saveThread: () => Effect.void,
        removeThread: () => Effect.void,
        clear: () => Effect.void,
      });
      const shellState = yield* makeEnvironmentShellState().pipe(
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.provideService(Persistence.EnvironmentCacheStore, cache),
      );
      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "connected",
        stage: null,
        attempt: 1,
        generation: 1,
        lastFailure: null,
        retryAt: null,
      });
      yield* yieldForShell;

      const statuses: ReadonlyArray<EnvironmentAvailabilityStatus> = [
        "starting",
        "synchronizing",
        "degraded",
        "storage-changed",
        "unavailable",
      ];
      const deltaSequences = [5, 4, 3] as const;
      for (const projects of [[], [CACHED_PROJECT]] as const) {
        const cachedSnapshot: OrchestrationShellSnapshot = {
          snapshotSequence: 4,
          projects,
          threads: [],
          updatedAt: "2026-06-01T00:00:00.000Z",
        };
        for (const status of statuses) {
          for (const sequence of deltaSequences) {
            yield* SubscriptionRef.set(shellState, {
              snapshot: Option.some(cachedSnapshot),
              status,
              error: Option.none(),
            });
            yield* Queue.offer(events, {
              kind: "project-removed",
              sequence,
              projectId: CACHED_PROJECT.id,
            });
            yield* yieldForShell;
            const current = yield* SubscriptionRef.get(shellState);
            expect(current.status).toBe(status);
            expect(Option.getOrThrow(current.snapshot)).toBe(cachedSnapshot);
            expect(Option.getOrThrow(current.snapshot).projects).toEqual(projects);
          }
        }
      }

      yield* TestClock.adjust("1 second");
      expect(yield* Ref.get(saveCount)).toBe(0);
    }),
  );

  it.effect("starts synchronization when the session is published after connected state", () =>
    Effect.gen(function* () {
      const events = yield* Queue.unbounded<OrchestrationShellStreamItem>();
      const currentSession = session({
        [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(events),
      } as unknown as WsRpcProtocolClient);
      const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
      const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
        Option.none(),
      );
      const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
        target: TARGET,
        state: supervisorState,
        session: activeSession,
        prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
        connect: Effect.void,
        disconnect: Effect.void,
        retryNow: Effect.void,
      } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
      const cache = Persistence.EnvironmentCacheStore.of({
        loadShell: () => Effect.succeed(Option.none()),
        saveShell: () => Effect.void,
        loadThread: () => Effect.succeed(Option.none()),
        saveThread: () => Effect.void,
        removeThread: () => Effect.void,
        clear: () => Effect.void,
      });
      const shellState = yield* makeEnvironmentShellState().pipe(
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.provideService(Persistence.EnvironmentCacheStore, cache),
      );

      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "connected",
        stage: null,
        attempt: 1,
        generation: 1,
        lastFailure: null,
        retryAt: null,
      });
      yield* yieldForShell;
      expect((yield* SubscriptionRef.get(shellState)).status).toBe("synchronizing");

      yield* SubscriptionRef.set(activeSession, Option.some(currentSession));
      yield* yieldForShell;
      yield* Queue.offer(events, {
        kind: "snapshot",
        snapshot: LIVE_SHELL_SNAPSHOT,
      });
      yield* yieldForShell;
      const live = yield* SubscriptionRef.get(shellState);
      expect(live.status).toBe("live");
      expect(Option.getOrThrow(live.snapshot)).toBe(LIVE_SHELL_SNAPSHOT);
    }),
  );

  it.effect(
    "requires a full snapshot again after reconnect before accepting current-session deltas",
    () =>
      Effect.gen(function* () {
        const firstEvents = yield* Queue.unbounded<OrchestrationShellStreamItem>();
        const nextEvents = yield* Queue.unbounded<OrchestrationShellStreamItem>();
        const firstSession = session({
          [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(firstEvents),
        } as unknown as WsRpcProtocolClient);
        const nextSession = session({
          [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(nextEvents),
        } as unknown as WsRpcProtocolClient);
        const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
        const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
          Option.some(firstSession),
        );
        const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
          target: TARGET,
          state: supervisorState,
          session: activeSession,
          prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
          connect: Effect.void,
          disconnect: Effect.void,
          retryNow: Effect.void,
        } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
        const savedSequences = yield* Ref.make<ReadonlyArray<number>>([]);
        const cache = Persistence.EnvironmentCacheStore.of({
          loadShell: () => Effect.succeed(Option.none()),
          saveShell: (_environmentId, snapshot) =>
            Ref.update(savedSequences, (sequences) => [...sequences, snapshot.snapshotSequence]),
          loadThread: () => Effect.succeed(Option.none()),
          saveThread: () => Effect.void,
          removeThread: () => Effect.void,
          clear: () => Effect.void,
        });
        const shellState = yield* makeEnvironmentShellState().pipe(
          Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
          Effect.provideService(Persistence.EnvironmentCacheStore, cache),
        );
        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "connected",
          stage: null,
          attempt: 1,
          generation: 1,
          lastFailure: null,
          retryAt: null,
        });
        yield* yieldForShell;
        yield* Queue.offer(firstEvents, {
          kind: "snapshot",
          snapshot: { ...LIVE_SHELL_SNAPSHOT, projects: [CACHED_PROJECT] },
        });
        yield* SubscriptionRef.changes(shellState).pipe(
          Stream.filter((state) => state.status === "live"),
          Stream.runHead,
        );
        yield* Queue.offer(firstEvents, {
          kind: "project-removed",
          sequence: 2,
          projectId: CACHED_PROJECT.id,
        });
        yield* SubscriptionRef.changes(shellState).pipe(
          Stream.filter(
            (state) => Option.isSome(state.snapshot) && state.snapshot.value.snapshotSequence === 2,
          ),
          Stream.runHead,
        );
        yield* TestClock.adjust("1 second");
        const savesBeforeReconnectDelta = yield* Ref.get(savedSequences);

        yield* SubscriptionRef.set(activeSession, Option.some(nextSession));
        yield* yieldForShell;
        const invalidatedSession = yield* SubscriptionRef.get(shellState);
        expect(invalidatedSession.status).toBe("synchronizing");
        yield* Queue.offer(firstEvents, {
          kind: "project-upserted",
          sequence: 3,
          project: { ...CACHED_PROJECT, title: "Stale prior-session project" },
        });
        yield* yieldForShell;
        expect(yield* SubscriptionRef.get(shellState)).toBe(invalidatedSession);
        yield* Queue.offer(nextEvents, {
          kind: "snapshot",
          snapshot: {
            ...LIVE_SHELL_SNAPSHOT,
            projects: [{ ...CACHED_PROJECT, title: "Premature next-session project" }],
          },
        });
        yield* yieldForShell;
        expect(yield* SubscriptionRef.get(shellState)).toBe(invalidatedSession);
        yield* Queue.take(nextEvents);

        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "connecting",
          stage: "synchronizing",
          attempt: 2,
          generation: 2,
          lastFailure: null,
          retryAt: null,
        });
        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "connected",
          stage: null,
          attempt: 2,
          generation: 2,
          lastFailure: null,
          retryAt: null,
        });
        yield* yieldForShell;
        const cachedBeforeDelta = yield* SubscriptionRef.get(shellState);
        expect(cachedBeforeDelta.status).toBe("synchronizing");

        yield* Queue.offer(nextEvents, {
          kind: "project-upserted",
          sequence: 3,
          project: { ...CACHED_PROJECT, title: "Must not appear before the snapshot" },
        });
        yield* yieldForShell;
        yield* TestClock.adjust("1 second");
        const awaitingSnapshot = yield* SubscriptionRef.get(shellState);
        expect(awaitingSnapshot).toBe(cachedBeforeDelta);
        expect(awaitingSnapshot.status).toBe("synchronizing");
        expect(Option.getOrThrow(awaitingSnapshot.snapshot).projects).toEqual([]);
        expect(yield* Ref.get(savedSequences)).toEqual(savesBeforeReconnectDelta);

        yield* Queue.offer(nextEvents, {
          kind: "snapshot",
          snapshot: { ...LIVE_SHELL_SNAPSHOT, snapshotSequence: 1 },
        });
        yield* SubscriptionRef.changes(shellState).pipe(
          Stream.filter((state) => state.status === "live"),
          Stream.runHead,
        );
        yield* Queue.offer(nextEvents, {
          kind: "project-upserted",
          sequence: 2,
          project: { ...CACHED_PROJECT, title: "Current project" },
        });
        yield* SubscriptionRef.changes(shellState).pipe(
          Stream.filter(
            (state) => Option.isSome(state.snapshot) && state.snapshot.value.snapshotSequence === 2,
          ),
          Stream.runHead,
        );
        yield* TestClock.adjust("1 second");
        const liveAfterDelta = yield* SubscriptionRef.get(shellState);
        expect(liveAfterDelta.status).toBe("live");
        expect(
          Option.getOrThrow(liveAfterDelta.snapshot).projects.map((project) => project.title),
        ).toEqual(["Current project"]);
        const savesBeforeIgnoredDeltas = yield* Ref.get(savedSequences);

        yield* Queue.offer(nextEvents, {
          kind: "project-removed",
          sequence: 2,
          projectId: CACHED_PROJECT.id,
        });
        yield* Queue.offer(nextEvents, {
          kind: "project-removed",
          sequence: 1,
          projectId: CACHED_PROJECT.id,
        });
        yield* yieldForShell;
        yield* TestClock.adjust("1 second");
        expect(yield* SubscriptionRef.get(shellState)).toBe(liveAfterDelta);
        expect(yield* Ref.get(savedSequences)).toEqual(savesBeforeIgnoredDeltas);
      }),
  );

  it.effect("clears a blocked storage error when the connection becomes generically degraded", () =>
    Effect.gen(function* () {
      const client = {
        [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.never,
      } as unknown as WsRpcProtocolClient;
      const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
      const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
        Option.some(session(client)),
      );
      const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
        target: TARGET,
        state: supervisorState,
        session: activeSession,
        prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
        connect: Effect.void,
        disconnect: Effect.void,
        retryNow: Effect.void,
      } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
      const cache = Persistence.EnvironmentCacheStore.of({
        loadShell: () => Effect.succeed(Option.some(LIVE_SHELL_SNAPSHOT)),
        saveShell: () => Effect.void,
        loadThread: () => Effect.succeed(Option.none()),
        saveThread: () => Effect.void,
        removeThread: () => Effect.void,
        clear: () => Effect.void,
      });
      const shellState = yield* makeEnvironmentShellState().pipe(
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.provideService(Persistence.EnvironmentCacheStore, cache),
      );
      const storageFailure = new ConnectionStorageChangedError({
        reason: "storage-changed",
        detail: "The old storage location is blocked.",
        targetKey: "platform:primary",
        acceptedStorageInstanceId: "accepted",
        reportedStorageInstanceId: "reported",
      });
      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "blocked",
        stage: null,
        attempt: 1,
        generation: 0,
        lastFailure: storageFailure,
        retryAt: null,
      });
      yield* yieldForShell;
      expect(Option.getOrNull((yield* SubscriptionRef.get(shellState)).error)).toBe(
        storageFailure.message,
      );

      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "backoff",
        stage: null,
        attempt: 2,
        generation: 0,
        lastFailure: new ConnectionTransientError({
          reason: "transport",
          detail: "Retrying after transport loss.",
        }),
        retryAt: 1_000,
      });
      yield* yieldForShell;
      const degraded = yield* SubscriptionRef.get(shellState);
      expect(degraded.status).toBe("degraded");
      expect(Option.isNone(degraded.error)).toBe(true);
    }),
  );

  it.effect("publishes live state before persistence and preserves it when ready", () =>
    Effect.gen(function* () {
      const events = yield* Queue.unbounded<OrchestrationShellStreamItem>();
      const client = {
        [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(events),
      } as unknown as WsRpcProtocolClient;
      const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
      const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
        Option.some(session(client)),
      );
      const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
        target: TARGET,
        state: supervisorState,
        session: activeSession,
        prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
        connect: Effect.void,
        disconnect: Effect.void,
        retryNow: Effect.void,
      } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
      const cache = Persistence.EnvironmentCacheStore.of({
        loadShell: () => Effect.succeed(Option.none()),
        saveShell: () => Effect.never,
        loadThread: () => Effect.succeed(Option.none()),
        saveThread: () => Effect.void,
        removeThread: () => Effect.void,
        clear: () => Effect.void,
      });
      const shellState = yield* makeEnvironmentShellState().pipe(
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.provideService(Persistence.EnvironmentCacheStore, cache),
      );

      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "connecting",
        stage: "synchronizing",
        attempt: 1,
        generation: 0,
        lastFailure: null,
        retryAt: null,
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }
      expect((yield* SubscriptionRef.get(shellState)).status).toBe("synchronizing");
      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "offline",
        phase: "offline",
        stage: null,
        attempt: 1,
        generation: 0,
        lastFailure: null,
        retryAt: null,
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }
      expect((yield* SubscriptionRef.get(shellState)).status).toBe("unavailable");
      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "connected",
        stage: null,
        attempt: 1,
        generation: 0,
        lastFailure: null,
        retryAt: null,
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }
      expect((yield* SubscriptionRef.get(shellState)).status).toBe("synchronizing");
      yield* Queue.offer(events, {
        kind: "project-removed",
        sequence: 1,
        projectId: ProjectId.make("missing-project"),
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }
      expect(Option.isNone((yield* SubscriptionRef.get(shellState)).snapshot)).toBe(true);

      yield* Queue.offer(events, {
        kind: "snapshot",
        snapshot: LIVE_SHELL_SNAPSHOT,
      });
      yield* SubscriptionRef.changes(shellState).pipe(
        Stream.filter((state) => state.status === "live"),
        Stream.runHead,
      );
      yield* Queue.offer(events, {
        kind: "project-removed",
        sequence: 1,
        projectId: ProjectId.make("missing-project"),
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }
      expect(Option.getOrThrow((yield* SubscriptionRef.get(shellState)).snapshot)).toBe(
        LIVE_SHELL_SNAPSHOT,
      );
      yield* Queue.offer(events, {
        kind: "project-removed",
        sequence: 2,
        projectId: ProjectId.make("missing-project"),
      });
      yield* SubscriptionRef.changes(shellState).pipe(
        Stream.filter(
          (next) => Option.isSome(next.snapshot) && next.snapshot.value.snapshotSequence === 2,
        ),
        Stream.runHead,
      );

      yield* SubscriptionRef.set(supervisorState, {
        desired: true,
        network: "online",
        phase: "connected",
        stage: null,
        attempt: 1,
        generation: 1,
        lastFailure: null,
        retryAt: null,
      });
      for (let index = 0; index < 10; index += 1) {
        yield* Effect.yieldNow;
      }

      let state = yield* SubscriptionRef.get(shellState);
      expect(state.status).toBe("synchronizing");
      expect(Option.getOrThrow(state.snapshot).snapshotSequence).toBe(2);

      yield* Queue.offer(events, {
        kind: "snapshot",
        snapshot: LIVE_SHELL_SNAPSHOT,
      });
      yield* SubscriptionRef.changes(shellState).pipe(
        Stream.filter((next) => next.status === "live"),
        Stream.runHead,
      );
      state = yield* SubscriptionRef.get(shellState);
      expect(state.status).toBe("live");
      expect(Option.getOrThrow(state.snapshot)).toBe(LIVE_SHELL_SNAPSHOT);
    }),
  );

  it.effect(
    "retains cached projects through a storage mismatch until an adopted live empty snapshot arrives",
    () =>
      Effect.gen(function* () {
        const cachedSnapshot: OrchestrationShellSnapshot = {
          snapshotSequence: 4,
          projects: [
            {
              id: ProjectId.make("cached-project"),
              title: "Cached project",
              workspaceRoot: "/cached/project",
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
          threads: [],
          updatedAt: "2026-06-01T00:00:00.000Z",
        };
        const events = yield* Queue.unbounded<OrchestrationShellStreamItem>();
        const client = {
          [ORCHESTRATION_WS_METHODS.subscribeShell]: () => Stream.fromQueue(events),
        } as unknown as WsRpcProtocolClient;
        const supervisorState = yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE);
        const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
          Option.some(session(client)),
        );
        const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
          target: TARGET,
          state: supervisorState,
          session: activeSession,
          prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
          connect: Effect.void,
          disconnect: Effect.void,
          retryNow: Effect.void,
        } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
        const cache = Persistence.EnvironmentCacheStore.of({
          loadShell: () => Effect.succeed(Option.some(cachedSnapshot)),
          saveShell: () => Effect.void,
          loadThread: () => Effect.succeed(Option.none()),
          saveThread: () => Effect.void,
          removeThread: () => Effect.void,
          clear: () => Effect.void,
        });
        const shellState = yield* makeEnvironmentShellState().pipe(
          Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
          Effect.provideService(Persistence.EnvironmentCacheStore, cache),
        );

        expect((yield* SubscriptionRef.get(shellState)).status).toBe("degraded");
        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "blocked",
          stage: null,
          attempt: 1,
          generation: 0,
          lastFailure: new ConnectionStorageChangedError({
            reason: "storage-changed",
            detail: "The environment reported a different persistent store.",
            targetKey: "platform:primary",
            acceptedStorageInstanceId: "11111111-1111-4111-8111-111111111111",
            reportedStorageInstanceId: "22222222-2222-4222-8222-222222222222",
          }),
          retryAt: null,
        });
        yield* Effect.yieldNow;
        yield* Effect.yieldNow;
        let current = yield* SubscriptionRef.get(shellState);
        expect(current.status).toBe("storage-changed");
        expect(
          Option.getOrThrow(current.snapshot).projects.map((project) => project.title),
        ).toEqual(["Cached project"]);

        // Explicit adoption schedules the normal retry. The accepted cache is
        // retained while that retry synchronizes the newly accepted store.
        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "connecting",
          stage: "synchronizing",
          attempt: 2,
          generation: 0,
          lastFailure: null,
          retryAt: null,
        });
        yield* Effect.yieldNow;
        yield* Effect.yieldNow;
        current = yield* SubscriptionRef.get(shellState);
        expect(current.status).toBe("synchronizing");
        expect(Option.getOrThrow(current.snapshot).projects).toHaveLength(1);

        yield* SubscriptionRef.set(supervisorState, {
          desired: true,
          network: "online",
          phase: "connected",
          stage: null,
          attempt: 2,
          generation: 1,
          lastFailure: null,
          retryAt: null,
        });
        yield* yieldForShell;
        yield* Queue.offer(events, {
          kind: "snapshot",
          snapshot: {
            snapshotSequence: 1,
            projects: [],
            threads: [],
            updatedAt: "2026-06-07T00:00:00.000Z",
          },
        });
        yield* SubscriptionRef.changes(shellState).pipe(
          Stream.filter((state) => state.status === "live"),
          Stream.runHead,
        );
        current = yield* SubscriptionRef.get(shellState);
        expect(current.status).toBe("live");
        expect(Option.getOrThrow(current.snapshot).projects).toEqual([]);
      }),
  );
});
