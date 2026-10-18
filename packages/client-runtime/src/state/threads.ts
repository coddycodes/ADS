import {
  ORCHESTRATION_WS_METHODS,
  type EnvironmentId as EnvironmentIdType,
  type OrchestrationThread,
  type OrchestrationThreadStreamItem,
  type ThreadId as ThreadIdType,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { Atom } from "effect/unstable/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { connectionProjectionPhase } from "../connection/model.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { EnvironmentCacheStore } from "../platform/persistence.ts";
import { subscribe } from "../rpc/client.ts";
import { parseThreadKey, threadKey } from "./entities.ts";
import { applyThreadPatch } from "./threadPatch.ts";
import { applyThreadDetailEvent } from "./threadReducer.ts";
import { THREAD_STATE_IDLE_TTL_MS } from "./threadRetention.ts";
import { followStreamInEnvironment } from "./runtime.ts";

export type EnvironmentThreadStatus = "empty" | "cached" | "synchronizing" | "live" | "deleted";

export interface EnvironmentThreadState {
  readonly data: Option.Option<OrchestrationThread>;
  readonly status: EnvironmentThreadStatus;
  readonly error: Option.Option<string>;
}

export const EMPTY_ENVIRONMENT_THREAD_STATE: EnvironmentThreadState = {
  data: Option.none(),
  status: "empty",
  error: Option.none(),
};

function statusWithoutLiveData(data: Option.Option<OrchestrationThread>): EnvironmentThreadStatus {
  return Option.isSome(data) ? "cached" : "empty";
}

function formatThreadError(cause: Cause.Cause<unknown>): string {
  const error = Cause.squash(cause);
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : "Could not synchronize the thread.";
}

export const makeEnvironmentThreadState = Effect.fn("EnvironmentThreadState.make")(function* (
  threadId: ThreadIdType,
) {
  const supervisor = yield* EnvironmentSupervisor;
  const cache = yield* EnvironmentCacheStore;
  const environmentId = supervisor.target.environmentId;
  const cached = yield* cache.loadThread(environmentId, threadId).pipe(
    Effect.catch((error) =>
      Effect.logWarning("Could not load cached thread.").pipe(
        Effect.annotateLogs({
          environmentId,
          threadId,
          error: error.message,
        }),
        Effect.as(Option.none<OrchestrationThread>()),
      ),
    ),
  );
  const state = yield* SubscriptionRef.make<EnvironmentThreadState>({
    data: cached,
    status: statusWithoutLiveData(cached),
    error: Option.none(),
  });
  const lastSequence = yield* SubscriptionRef.make(0);
  // The server's revision of the thread this state holds, after the last item applied in full.
  // A resubscribe sends it so the server can answer `unchanged` instead of the whole thread; any
  // change the server did not describe with a revision clears it.
  let resumeFrom: { readonly revision: string; readonly snapshotSequence: number } | null = null;
  const resumeAt = (revision: string | undefined, snapshotSequence: number) => {
    resumeFrom = revision === undefined ? null : { revision, snapshotSequence };
  };
  const persistence = yield* Queue.sliding<OrchestrationThread>(1);

  const persist = Effect.fn("EnvironmentThreadState.persist")(function* (
    thread: OrchestrationThread,
  ) {
    yield* cache.saveThread(environmentId, thread).pipe(
      Effect.catch((error) =>
        Effect.logWarning("Could not persist the thread cache.").pipe(
          Effect.annotateLogs({
            environmentId,
            threadId,
            error: error.message,
          }),
        ),
      ),
    );
  });

  yield* Stream.fromQueue(persistence).pipe(
    Stream.debounce("500 millis"),
    Stream.runForEach(persist),
    Effect.forkScoped,
  );

  const setSynchronizing = SubscriptionRef.update(state, (current) => ({
    ...current,
    status: "synchronizing" as const,
    error: Option.none(),
  }));
  const setReady = SubscriptionRef.update(state, (current) =>
    current.status === "live" || current.status === "deleted"
      ? current
      : {
          ...current,
          status: "synchronizing" as const,
          error: Option.none(),
        },
  );
  const setDisconnected = SubscriptionRef.update(state, (current) => ({
    ...current,
    status: current.status === "deleted" ? current.status : statusWithoutLiveData(current.data),
  }));
  const setStreamError = (cause: Cause.Cause<unknown>) =>
    SubscriptionRef.update(state, (current) => ({
      ...current,
      status: current.status === "deleted" ? current.status : statusWithoutLiveData(current.data),
      error: Option.some(formatThreadError(cause)),
    }));

  const setThread = Effect.fn("EnvironmentThreadState.setThread")(function* (
    thread: OrchestrationThread,
  ) {
    yield* SubscriptionRef.set(state, {
      data: Option.some(thread),
      status: "live",
      error: Option.none(),
    });
    yield* Queue.offer(persistence, thread);
  });

  const setDeleted = Effect.fn("EnvironmentThreadState.setDeleted")(function* () {
    resumeFrom = null;
    yield* SubscriptionRef.set(state, {
      data: Option.none(),
      status: "deleted",
      error: Option.none(),
    });
    yield* cache.removeThread(environmentId, threadId).pipe(
      Effect.catch((error) =>
        Effect.logWarning("Could not remove the cached thread.").pipe(
          Effect.annotateLogs({
            environmentId,
            threadId,
            error: error.message,
          }),
        ),
      ),
    );
  });

  const applyItem = Effect.fn("EnvironmentThreadState.applyItem")(function* (
    item: OrchestrationThreadStreamItem,
  ) {
    if (item.kind === "snapshot") {
      yield* SubscriptionRef.set(lastSequence, item.snapshot.snapshotSequence);
      yield* setThread(item.snapshot.thread);
      resumeAt(item.revision, item.snapshot.snapshotSequence);
      return;
    }
    if (item.kind === "patch") {
      // Computed against the snapshot this subscription holds, so it always applies.
      const current = yield* SubscriptionRef.get(state);
      if (Option.isNone(current.data)) {
        resumeFrom = null;
        return;
      }
      yield* SubscriptionRef.set(lastSequence, item.snapshotSequence);
      yield* setThread(applyThreadPatch(current.data.value, item.patch));
      resumeAt(item.revision, item.snapshotSequence);
      return;
    }
    if (item.kind === "unchanged") {
      // The server still has the revision this state holds: keep the thread, it is live again.
      yield* SubscriptionRef.set(lastSequence, item.snapshotSequence);
      yield* SubscriptionRef.update(state, (current) =>
        Option.isNone(current.data)
          ? current
          : { ...current, status: "live" as const, error: Option.none() },
      );
      resumeAt(item.revision, item.snapshotSequence);
      return;
    }
    resumeFrom = null;

    const sequence = yield* SubscriptionRef.get(lastSequence);
    if (item.event.sequence <= sequence) {
      return;
    }
    yield* SubscriptionRef.set(lastSequence, item.event.sequence);

    const current = yield* SubscriptionRef.get(state);
    if (Option.isNone(current.data)) {
      if (item.event.type === "thread.deleted") {
        yield* setDeleted();
      }
      return;
    }
    const result = applyThreadDetailEvent(current.data.value, item.event);
    if (result.kind === "updated") {
      yield* setThread(result.thread);
    } else if (result.kind === "deleted") {
      yield* setDeleted();
    }
  });

  yield* SubscriptionRef.changes(supervisor.state).pipe(
    Stream.runForEach((connectionState) => {
      switch (connectionProjectionPhase(connectionState)) {
        case "synchronizing":
          return setSynchronizing;
        case "disconnected":
          return setDisconnected;
        case "ready":
          return setReady;
      }
    }),
    Effect.forkScoped,
  );

  yield* setSynchronizing;
  yield* subscribe(
    ORCHESTRATION_WS_METHODS.subscribeThread,
    // Updates arrive as patches against the snapshot this subscription holds; a resubscribe
    // names the revision it holds, so an unchanged thread is not sent again.
    () =>
      resumeFrom === null
        ? { threadId, patches: true }
        : { threadId, patches: true, resume: resumeFrom },
    {
      onExpectedFailure: setStreamError,
      // A thread can appear just after its view subscribes, so a failure is retried soon. A
      // deleted thread fails every time; a fixed 250 ms retry hammered the server (#113).
      retryExpectedFailure: {
        when: () => true,
        initialDelay: "250 millis",
        maxDelay: "30 seconds",
        resetAfter: "30 seconds",
      },
    },
  ).pipe(Stream.runForEach(applyItem), Effect.forkScoped);

  yield* Effect.addFinalizer(() =>
    SubscriptionRef.get(state).pipe(
      Effect.flatMap((current) =>
        Option.match(current.data, {
          onNone: () => Effect.void,
          onSome: persist,
        }),
      ),
    ),
  );

  return state;
});

export function threadStateChanges(environmentId: EnvironmentIdType, threadId: ThreadIdType) {
  return followStreamInEnvironment(
    environmentId,
    Stream.unwrap(makeEnvironmentThreadState(threadId).pipe(Effect.map(SubscriptionRef.changes))),
  );
}

export function createEnvironmentThreadStateAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | EnvironmentCacheStore | R, E>,
) {
  const family = Atom.family((key: string) => {
    const { environmentId, threadId } = parseThreadKey(key);
    return runtime
      .atom(threadStateChanges(environmentId, threadId), {
        initialValue: EMPTY_ENVIRONMENT_THREAD_STATE,
      })
      .pipe(
        Atom.setIdleTTL(THREAD_STATE_IDLE_TTL_MS),
        Atom.withLabel(`environment-thread-state:${key}`),
      );
  });

  return {
    stateAtom: (environmentId: EnvironmentIdType, threadId: ThreadIdType) =>
      family(threadKey({ environmentId, threadId })),
  };
}

export * from "./archivedThreads.ts";
export * from "./checkpointDiff.ts";
export * from "./composerPathSearch.ts";
export * from "./threadCommands.ts";
export * from "./threadDetail.ts";
export * from "./threadReducer.ts";
export * from "./threadShell.ts";
