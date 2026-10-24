import { afterEach, vi } from "vite-plus/test";
import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId, type RemoteUpdateSnapshot, WS_METHODS } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";

import { EnvironmentRegistry } from "../connection/registry.ts";
import {
  ConnectionBlockedError,
  RelayConnectionTarget,
  type SupervisorConnectionState,
} from "../connection/model.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { EnvironmentRpcUnavailableError } from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { AtomCommandResult } from "./runtime.ts";
import {
  MAX_CONCURRENT_REMOTE_UPDATE_CHECKS,
  REMOTE_UPDATE_BUSY_REFRESH_MS,
  REMOTE_UPDATE_CHECK_TIMEOUT_MS,
  REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS,
  createRemoteUpdateEnvironmentAtoms,
  fanOutRemoteUpdateChecks,
  isRemoteUpdateAvailable,
  isRemoteUpdateConnectionFailure,
  isRemoteUpdateUnchecked,
  remoteUpdateStatusRefreshIntervalMs,
} from "./remoteUpdates.ts";

const flushMicrotasks = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

const settledSuccess = <A>(value: A): AtomCommandResult<A, never> =>
  ({ _tag: "Success", value }) as unknown as AtomCommandResult<A, never>;
const settledFailure = <E>(error: E): AtomCommandResult<never, E> =>
  ({ _tag: "Failure", cause: { error } }) as unknown as AtomCommandResult<never, E>;

const CHECKED_SNAPSHOT: RemoteUpdateSnapshot = {
  serverVersion: "0.4.2",
  latestVersion: "0.5.0",
  state: "update-available",
  error: null,
  support: { installMode: "interactive", reason: "available", installKind: "unknown" },
  downloadPercent: null,
  targetVersion: null,
  installStage: null,
};

const makeRemoteUpdateCommandHarness = Effect.fn("TestRemoteUpdates.makeCommandHarness")(
  function* (options: {
    readonly acquire: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly check: (environmentId: EnvironmentId) => Effect.Effect<RemoteUpdateSnapshot>;
  }) {
    const clock = yield* TestClock.make({ warningDelay: "1 hour" });
    const atomRegistry = AtomRegistry.make();
    yield* Effect.addFinalizer(() => Effect.sync(() => atomRegistry.dispose()));

    const run: EnvironmentRegistry["Service"]["run"] = (environmentId, effect) =>
      options.acquire(environmentId).pipe(
        Effect.andThen(
          Effect.gen(function* () {
            const session = {
              client: {
                [WS_METHODS.updaterCheck]: () => options.check(environmentId),
              },
            } as unknown as RpcSession;
            const sessionRef = yield* SubscriptionRef.make(Option.some(session));
            const supervisor = EnvironmentSupervisor.of({
              target: new RelayConnectionTarget({
                environmentId,
                label: `Environment ${environmentId}`,
              }),
              session: sessionRef,
            } as EnvironmentSupervisor["Service"]);
            return yield* Effect.provideService(effect, EnvironmentSupervisor, supervisor);
          }),
        ),
      );
    const registry = EnvironmentRegistry.of({ run } as EnvironmentRegistry["Service"]);
    const runtime = Atom.runtime(
      Layer.merge(Layer.succeed(EnvironmentRegistry, registry), Layer.succeed(Clock.Clock, clock)),
    );
    return {
      atomRegistry,
      check: createRemoteUpdateEnvironmentAtoms(runtime).check,
      clock,
    };
  },
);

function expectCompletedWithTimeout(
  exit: ReturnType<Fiber.Fiber<AtomCommandResult<unknown, unknown>, never>["pollUnsafe"]>,
): void {
  expect(exit?._tag).toBe("Success");
  if (exit?._tag !== "Success") return;
  expect(exit.value._tag).toBe("Failure");
  if (exit.value._tag !== "Failure") return;
  expect(Cause.squash(exit.value.cause)).toMatchObject({ _tag: "TimeoutError" });
}

describe("fanOutRemoteUpdateChecks", () => {
  it("exports the spec-pinned limit of two", () => {
    expect(MAX_CONCURRENT_REMOTE_UPDATE_CHECKS).toBe(2);
  });

  it("never runs more than two checks at once and preserves input order", async () => {
    const ids = ["env-a", "env-b", "env-c", "env-d"].map((id) => EnvironmentId.make(id));
    const releasers = new Map<string, () => void>();
    let inFlight = 0;
    let peak = 0;

    const batch = fanOutRemoteUpdateChecks(ids, (environmentId) => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      return new Promise<AtomCommandResult<string, never>>((resolve) => {
        releasers.set(environmentId, () => {
          inFlight -= 1;
          resolve(settledSuccess(`checked:${environmentId}`));
        });
      });
    });

    await flushMicrotasks();
    expect(releasers.size).toBe(2);
    expect(peak).toBe(2);

    releasers.get(ids[0]!)!();
    await flushMicrotasks();
    expect(releasers.size).toBe(3);
    expect(peak).toBe(2);

    for (const release of releasers.values()) release();
    await flushMicrotasks();
    for (const release of releasers.values()) release();

    const results = await batch;
    expect(results.map((result) => result.environmentId)).toEqual(ids);
    expect(results.every((result) => result.outcome.kind === "success")).toBe(true);
    expect(peak).toBe(2);
  });

  it("classifies a settled Failure VALUE as a failure, not a success", async () => {
    const ids = ["env-a", "env-b", "env-c"].map((id) => EnvironmentId.make(id));
    const results = await fanOutRemoteUpdateChecks(ids, (environmentId) =>
      environmentId === ids[1]
        ? Promise.resolve(settledFailure("unreachable"))
        : Promise.resolve(settledSuccess("ok")),
    );
    expect(results.map((result) => result.outcome.kind)).toEqual(["success", "failure", "success"]);
    const failure = results[1]!.outcome;
    expect(failure.kind === "failure" && failure.result?._tag).toBe("Failure");
  });

  it("also isolates a thrown rejection (defensive) instead of aborting the batch", async () => {
    const ids = ["env-a", "env-b"].map((id) => EnvironmentId.make(id));
    const results = await fanOutRemoteUpdateChecks(ids, (environmentId) =>
      environmentId === ids[0]
        ? Promise.reject(new Error("dispatcher blew up"))
        : Promise.resolve(settledSuccess("ok")),
    );
    expect(results.map((result) => result.outcome.kind)).toEqual(["failure", "success"]);
    const failure = results[0]!.outcome;
    expect(failure.kind === "failure" && failure.result).toBeNull();
    expect(failure.kind === "failure" && failure.error).toBeInstanceOf(Error);
  });

  it("handles an empty environment list", async () => {
    await expect(
      fanOutRemoteUpdateChecks([], () => Promise.resolve(settledSuccess("ok"))),
    ).resolves.toEqual([]);
  });
});

describe("remote update check timeout", () => {
  it.effect("bounds supervisor acquisition and does not invoke RPC after timeout", () =>
    Effect.gen(function* () {
      expect(REMOTE_UPDATE_CHECK_TIMEOUT_MS).toBe(30_000);
      const environmentId = EnvironmentId.make("environment-acquisition-stall");
      const acquisitionStarted = yield* Deferred.make<void>();
      const rpcInvocations = yield* Ref.make(0);
      const harness = yield* makeRemoteUpdateCommandHarness({
        acquire: () =>
          Deferred.succeed(acquisitionStarted, undefined).pipe(Effect.andThen(Effect.never)),
        check: () =>
          Ref.update(rpcInvocations, (count) => count + 1).pipe(Effect.as(CHECKED_SNAPSHOT)),
      });
      const fiber = yield* Effect.promise(() =>
        harness.check.run(harness.atomRegistry, { environmentId, input: {} }),
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(acquisitionStarted);
      yield* harness.clock.adjust(REMOTE_UPDATE_CHECK_TIMEOUT_MS);
      for (let iteration = 0; iteration < 100; iteration += 1) {
        yield* Effect.yieldNow;
      }

      expectCompletedWithTimeout(fiber.pollUnsafe());
      expect(yield* Ref.get(rpcInvocations)).toBe(0);
    }),
  );

  it.effect("bounds a stalled RPC after fast supervisor acquisition", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("environment-rpc-stall");
      const rpcStarted = yield* Deferred.make<void>();
      const harness = yield* makeRemoteUpdateCommandHarness({
        acquire: () => Effect.void,
        check: () => Deferred.succeed(rpcStarted, undefined).pipe(Effect.andThen(Effect.never)),
      });
      const fiber = yield* Effect.promise(() =>
        harness.check.run(harness.atomRegistry, { environmentId, input: {} }),
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(rpcStarted);
      yield* harness.clock.adjust(REMOTE_UPDATE_CHECK_TIMEOUT_MS);
      for (let iteration = 0; iteration < 100; iteration += 1) {
        yield* Effect.yieldNow;
      }

      expectCompletedWithTimeout(fiber.pollUnsafe());
    }),
  );

  it.effect("releases a fan-out worker slot when supervisor acquisition times out", () =>
    Effect.gen(function* () {
      const environmentIds = ["environment-a", "environment-b", "environment-c"].map((id) =>
        EnvironmentId.make(id),
      );
      const thirdAcquisitionStarted = yield* Deferred.make<void>();
      const started = yield* Ref.make<ReadonlyArray<EnvironmentId>>([]);
      const harness = yield* makeRemoteUpdateCommandHarness({
        acquire: (environmentId) =>
          Ref.update(started, (current) => [...current, environmentId]).pipe(
            Effect.andThen(
              environmentId === environmentIds[2]
                ? Deferred.succeed(thirdAcquisitionStarted, undefined)
                : Effect.never,
            ),
          ),
        check: () => Effect.succeed(CHECKED_SNAPSHOT),
      });
      const batch = yield* Effect.promise(() =>
        fanOutRemoteUpdateChecks(environmentIds, (environmentId) =>
          harness.check.run(harness.atomRegistry, { environmentId, input: {} }),
        ),
      ).pipe(Effect.forkChild({ startImmediately: true }));

      for (let iteration = 0; iteration < 100; iteration += 1) {
        yield* Effect.yieldNow;
      }
      expect(yield* Ref.get(started)).toEqual(environmentIds.slice(0, 2));

      yield* harness.clock.adjust(REMOTE_UPDATE_CHECK_TIMEOUT_MS);
      for (let iteration = 0; iteration < 100; iteration += 1) {
        yield* Effect.yieldNow;
      }

      expect(yield* Deferred.isDone(thirdAcquisitionStarted)).toBe(true);
      const exit = batch.pollUnsafe();
      expect(exit?._tag).toBe("Success");
      if (exit?._tag === "Success") {
        expect(exit.value.map((result) => result.environmentId)).toEqual(environmentIds);
        expect(exit.value.map((result) => result.outcome.kind)).toEqual([
          "failure",
          "failure",
          "success",
        ]);
      }
    }),
  );
});

describe("isRemoteUpdateAvailable", () => {
  it("is true only for update-available snapshots", () => {
    expect(isRemoteUpdateAvailable(CHECKED_SNAPSHOT)).toBe(true);
    expect(isRemoteUpdateAvailable({ ...CHECKED_SNAPSHOT, state: "up-to-date" })).toBe(false);
    expect(isRemoteUpdateAvailable({ ...CHECKED_SNAPSHOT, state: "error" })).toBe(false);
    expect(isRemoteUpdateAvailable(null)).toBe(false);
  });
});

function snapshotIn(
  state: RemoteUpdateSnapshot["state"],
  installMode: RemoteUpdateSnapshot["support"]["installMode"] = "interactive",
): RemoteUpdateSnapshot {
  return {
    ...CHECKED_SNAPSHOT,
    latestVersion: state === "update-available" ? "0.5.0" : null,
    state,
    error: state === "error" ? "feed unreachable" : null,
    support: {
      installMode,
      reason: installMode === "manual" ? "manual-update-required" : "available",
      installKind: "unknown",
    },
  };
}

describe("remoteUpdateStatusRefreshIntervalMs", () => {
  it("polls quickly while the host works and slowly until an automatic host has checked", () => {
    expect(REMOTE_UPDATE_BUSY_REFRESH_MS).toBeLessThanOrEqual(3_000);
    expect(REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS).toBe(30_000);
    for (const state of ["checking", "downloading", "installing"] as const) {
      expect(remoteUpdateStatusRefreshIntervalMs(snapshotIn(state))).toBe(
        REMOTE_UPDATE_BUSY_REFRESH_MS,
      );
    }
    expect(remoteUpdateStatusRefreshIntervalMs(snapshotIn("idle", "interactive"))).toBe(
      REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS,
    );
    expect(remoteUpdateStatusRefreshIntervalMs(snapshotIn("idle", "supervised"))).toBe(
      REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS,
    );
  });

  it("stops in terminal states and for manual hosts, whose status never changes by itself", () => {
    expect(remoteUpdateStatusRefreshIntervalMs(snapshotIn("idle", "manual"))).toBeNull();
    for (const state of ["update-available", "up-to-date", "error"] as const) {
      expect(remoteUpdateStatusRefreshIntervalMs(snapshotIn(state))).toBeNull();
    }
  });
});

class TestUpdaterUnreachable extends Schema.TaggedError<TestUpdaterUnreachable>()(
  "TestUpdaterUnreachable",
  { message: Schema.String },
) {}

const STATUS_ENVIRONMENT_ID = EnvironmentId.make("remote-update-status");
const STATUS_TARGET = { environmentId: STATUS_ENVIRONMENT_ID, input: {} };

/**
 * A connected environment whose `updater.status` answers with `respond(requestNumber)`
 * after a real asynchronous hop, like a network RPC. (A synchronous failure would also
 * trip `Atom.swr`'s first-read revalidation, which a real request never does.) The
 * snapshot atom is mounted, as a view would, until the test's scope closes.
 */
const makeStatusHarness = Effect.fn("TestRemoteUpdates.makeStatusHarness")(function* (
  respond: (request: number) => Effect.Effect<RemoteUpdateSnapshot, TestUpdaterUnreachable>,
) {
  const requests = { count: 0 };
  const networkHop = Effect.promise(() => new Promise<void>((resolve) => setImmediate(resolve)));
  const session = {
    client: {
      [WS_METHODS.updaterStatus]: () =>
        networkHop.pipe(
          Effect.andThen(
            Effect.suspend(() => {
              requests.count += 1;
              return respond(requests.count);
            }),
          ),
        ),
    },
  } as unknown as RpcSession;
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: STATUS_ENVIRONMENT_ID, label: "Remote update status test" },
    session: yield* SubscriptionRef.make(Option.some(session)),
    state: yield* SubscriptionRef.make({ phase: "connected", generation: 1 }),
  } as never);
  const environment = EnvironmentRegistry.of({
    run: <A, E>(_: EnvironmentId, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
      Effect.provideService(effect, EnvironmentSupervisor, supervisor),
    followStream: (
      _: EnvironmentId,
      stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>,
    ) => Stream.provideService(stream, EnvironmentSupervisor, supervisor),
  } as never);
  const atoms = createRemoteUpdateEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environment)),
  );
  const registry = AtomRegistry.make();
  yield* Effect.addFinalizer(() => Effect.sync(() => registry.dispose()));
  const atom = atoms.snapshot(STATUS_TARGET);
  const release = registry.mount(atom);
  let mounted = true;
  const unmount = () => {
    if (mounted) {
      mounted = false;
      release();
    }
  };
  yield* Effect.addFinalizer(() => Effect.sync(unmount));
  return {
    requests,
    state: () => settledState(registry.get(atom)),
    unmount,
  };
});

/** Lets fibers and atom propagation run; only `setTimeout` is faked, so immediates are real. */
const drainAtoms = Effect.promise(async () => {
  for (let turn = 0; turn < 50; turn += 1) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
});

const UPDATE_ENVIRONMENT_IDS = ["env-a", "env-b", "env-c"].map((id) => EnvironmentId.make(id));
const UPDATE_ENVIRONMENT_ID = UPDATE_ENVIRONMENT_IDS[0]!;
const UPDATE_TARGET = { environmentId: UPDATE_ENVIRONMENT_ID, input: {} };
const UP_TO_DATE_SNAPSHOT: RemoteUpdateSnapshot = { ...CHECKED_SNAPSHOT, state: "up-to-date" };
const UPDATE_CONNECTED_STATE: SupervisorConnectionState = {
  desired: true,
  network: "online",
  phase: "connected",
  stage: null,
  attempt: 0,
  generation: 1,
  lastFailure: null,
  retryAt: null,
};

const makeUpdateHarness = Effect.fn("TestRemoteUpdates.makeUpdateHarness")(function* (
  options: {
    readonly statusSequence?: ReadonlyArray<RemoteUpdateSnapshot["state"]>;
    readonly deferFollower?: boolean;
  } = {},
) {
  const clock = yield* TestClock.make({ warningDelay: "1 hour" });
  const atomRegistry = AtomRegistry.make();
  yield* Effect.addFinalizer(() => Effect.sync(() => atomRegistry.dispose()));
  const installs = new Map<EnvironmentId, number>();
  const statusReads = new Map<EnvironmentId, number>();
  const activeWorkReads = new Map<EnvironmentId, number>();
  const activeWorkCounts = { runningTurns: 2, liveTerminals: 3, queuedMessages: 1 };
  const configRequests = new Map<EnvironmentId, number>();
  const retries = new Map<EnvironmentId, number>();
  const followers = { active: 0 };
  const gates = new Map<EnvironmentId, Deferred.Deferred<void>>();
  const installations = new Map<
    EnvironmentId,
    SubscriptionRef.SubscriptionRef<EnvironmentSupervisor["Service"]>
  >();
  const followerReady = yield* Deferred.make<void>();
  if (!options.deferFollower) yield* Deferred.succeed(followerReady, undefined);

  const makeSupervisor = Effect.fn("TestRemoteUpdates.makeUpdateSupervisor")(function* (
    environmentId: EnvironmentId,
    bootId: string,
    serverVersion: string,
  ) {
    const initialConfig = Effect.succeed({
      environment: { bootId, serverVersion, capabilities: { remoteUpdateProgress: true } },
    });
    const session = {
      initialConfig,
      client: {
        [WS_METHODS.serverGetConfig]: () =>
          Effect.sync(() => {
            configRequests.set(environmentId, (configRequests.get(environmentId) ?? 0) + 1);
          }).pipe(Effect.andThen(initialConfig)),
        [WS_METHODS.updaterInstall]: () =>
          Effect.sync(() => {
            installs.set(environmentId, (installs.get(environmentId) ?? 0) + 1);
          }).pipe(
            Effect.andThen(Deferred.await(gates.get(environmentId)!)),
            Effect.as(
              options.statusSequence === undefined
                ? UP_TO_DATE_SNAPSHOT
                : { ...CHECKED_SNAPSHOT, state: "downloading" as const, downloadPercent: 25 },
            ),
          ),
        [WS_METHODS.updaterStatus]: () =>
          Effect.sync((): RemoteUpdateSnapshot => {
            const read = statusReads.get(environmentId) ?? 0;
            statusReads.set(environmentId, read + 1);
            const sequence = options.statusSequence ?? ["up-to-date"];
            return { ...CHECKED_SNAPSHOT, state: sequence[Math.min(read, sequence.length - 1)]! };
          }),
        [WS_METHODS.updaterActiveWork]: () =>
          Effect.sync(() => {
            activeWorkReads.set(environmentId, (activeWorkReads.get(environmentId) ?? 0) + 1);
            return { ...activeWorkCounts };
          }),
      },
    } as unknown as RpcSession;
    return EnvironmentSupervisor.of({
      target: new RelayConnectionTarget({ environmentId, label: `Environment ${environmentId}` }),
      session: yield* SubscriptionRef.make(Option.some(session)),
      state: yield* SubscriptionRef.make(UPDATE_CONNECTED_STATE),
    } as EnvironmentSupervisor["Service"]);
  });
  for (const environmentId of UPDATE_ENVIRONMENT_IDS) {
    gates.set(environmentId, yield* Deferred.make<void>());
    installations.set(
      environmentId,
      yield* SubscriptionRef.make(yield* makeSupervisor(environmentId, "boot-1", "0.4.2")),
    );
  }

  const run: EnvironmentRegistry["Service"]["run"] = (environmentId, effect) =>
    SubscriptionRef.get(installations.get(environmentId)!).pipe(
      Effect.flatMap((supervisor) =>
        Effect.provideService(effect, EnvironmentSupervisor, supervisor),
      ),
    );
  const followStream: EnvironmentRegistry["Service"]["followStream"] = (environmentId, stream) =>
    Stream.unwrap(
      Effect.acquireRelease(
        Effect.sync(() => {
          followers.active += 1;
        }),
        () =>
          Effect.sync(() => {
            followers.active -= 1;
          }),
      ).pipe(
        Effect.andThen(Deferred.await(followerReady)),
        Effect.as(
          SubscriptionRef.changes(installations.get(environmentId)!).pipe(
            Stream.switchMap((supervisor) =>
              Stream.provideService(stream, EnvironmentSupervisor, supervisor),
            ),
          ),
        ),
      ),
    );
  const environment: Pick<
    EnvironmentRegistry["Service"],
    "run" | "state" | "stateChanges" | "followStream" | "retryNow"
  > = {
    run,
    state: (environmentId) =>
      run(
        environmentId,
        EnvironmentSupervisor.pipe(
          Effect.flatMap((supervisor) => SubscriptionRef.get(supervisor.state)),
        ),
      ),
    stateChanges: (environmentId) =>
      followStream(
        environmentId,
        Stream.unwrap(
          EnvironmentSupervisor.pipe(
            Effect.map((supervisor) => SubscriptionRef.changes(supervisor.state)),
          ),
        ),
      ),
    followStream,
    retryNow: (environmentId) =>
      Effect.sync(() => {
        retries.set(environmentId, (retries.get(environmentId) ?? 0) + 1);
      }),
  };
  const runtime = Atom.runtime(
    Layer.merge(
      Layer.succeed(EnvironmentRegistry, environment as EnvironmentRegistry["Service"]),
      Layer.succeed(Clock.Clock, clock),
    ),
  );
  return {
    atomRegistry,
    atoms: createRemoteUpdateEnvironmentAtoms(runtime),
    clock,
    installs,
    statusReads,
    activeWorkReads,
    activeWorkCounts,
    configRequests,
    retries,
    followers,
    release: (environmentId: EnvironmentId) =>
      Deferred.succeed(gates.get(environmentId)!, undefined),
    setState: (state: SupervisorConnectionState) =>
      run(
        UPDATE_ENVIRONMENT_ID,
        EnvironmentSupervisor.pipe(
          Effect.flatMap((supervisor) => SubscriptionRef.set(supervisor.state, state)),
        ),
      ),
    replaceSupervisor: Effect.gen(function* () {
      const replacement = yield* makeSupervisor(UPDATE_ENVIRONMENT_ID, "boot-2", "0.5.0");
      yield* SubscriptionRef.set(installations.get(UPDATE_ENVIRONMENT_ID)!, replacement);
      yield* drainAtoms;
    }),
  };
});

describe("remote update active work", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it.effect("reads the active work when a view asks for it", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      const atom = h.atoms.activeWork(UPDATE_TARGET);
      yield* drainAtoms;
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBeUndefined();

      const unmount = h.atomRegistry.mount(atom);
      yield* drainAtoms;
      const result = h.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(result) && result.value).toEqual({
        runningTurns: 2,
        liveTerminals: 3,
        queuedMessages: 1,
      });
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      unmount();
    }),
  );

  it.effect("re-reads active work on every open", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      const atom = h.atoms.activeWork(UPDATE_TARGET);
      const unmount = h.atomRegistry.mount(atom);
      yield* drainAtoms;
      const first = h.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(first) && first.value).toEqual({
        runningTurns: 2,
        liveTerminals: 3,
        queuedMessages: 1,
      });
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBe(1);

      unmount();
      yield* drainAtoms;
      h.activeWorkCounts.liveTerminals = 4;

      const unmountAgain = h.atomRegistry.mount(atom);
      const reopened = h.atomRegistry.get(atom);
      expect(AsyncResult.isInitial(reopened) || reopened.waiting).toBe(true);

      yield* drainAtoms;
      const second = h.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(second) && second.value).toEqual({
        runningTurns: 2,
        liveTerminals: 4,
        queuedMessages: 1,
      });
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBe(2);
      unmountAgain();
    }),
  );

  it.effect("never polls active work", () =>
    Effect.gen(function* () {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const h = yield* makeUpdateHarness();
      const unmount = h.atomRegistry.mount(h.atoms.activeWork(UPDATE_TARGET));
      yield* drainAtoms;
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBe(1);

      yield* h.clock.adjust(60_000);
      yield* advance(60_000);
      expect(h.activeWorkReads.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      unmount();
    }),
  );
});

describe("remote update runs", () => {
  it.effect("runs at most two at once and shows a third as queued", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      const [a, b, c] = UPDATE_ENVIRONMENT_IDS as [EnvironmentId, EnvironmentId, EnvironmentId];
      for (const environmentId of [a, b, c]) {
        void h.atoms.update.run(h.atomRegistry, { environmentId, input: {} });
      }
      yield* drainAtoms;
      expect(h.installs.get(a)).toBe(1);
      expect(h.installs.get(b)).toBe(1);
      expect(h.installs.get(c)).toBeUndefined();
      expect(h.atomRegistry.get(h.atoms.run(c))).toEqual({ phase: "queued" });
      expect(h.followers.active).toBe(2);

      yield* h.release(a);
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(a))).toEqual({ phase: "up-to-date" });
      expect(h.installs.get(c)).toBe(1);
      expect(h.followers.active).toBe(2);
      yield* h.release(b);
      yield* h.release(c);
      yield* drainAtoms;
      expect(h.followers.active).toBe(0);
    }),
  );

  it.effect("joins a second request for the same environment instead of installing twice", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      const first = h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      const second = h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* drainAtoms;
      expect(h.installs.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      const result = yield* Effect.promise(() => first);
      expect(result).toMatchObject({ _tag: "Success", value: { phase: "up-to-date" } });
      expect(yield* Effect.promise(() => second)).toEqual(result);
    }),
  );

  it.effect("keeps running after the view that started it unmounts", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      const unmount = h.atomRegistry.mount(h.atoms.run(UPDATE_ENVIRONMENT_ID));
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* drainAtoms;
      unmount();
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "up-to-date",
      });
      expect(h.followers.active).toBe(0);
    }),
  );

  it.effect("never shows Restarting before the host actually disconnects", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({
        statusSequence: ["downloading", "downloading", "up-to-date"],
        deferFollower: true,
      });
      const seen: Array<string> = [];
      const unmount = h.atomRegistry.subscribe(h.atoms.run(UPDATE_ENVIRONMENT_ID), (state) => {
        if (state !== null) seen.push(state.phase);
      });
      yield* Effect.addFinalizer(() => Effect.sync(unmount));
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      expect(seen.at(-1)).toBe("downloading");
      for (let second = 0; second < 4; second += 1) {
        yield* h.clock.adjust("1 second");
        yield* drainAtoms;
      }
      expect(seen).not.toContain("restarting");
      expect(seen.at(-1)).toBe("up-to-date");
    }),
  );

  it.effect("verifies a rebuilt supervisor even when its generation restarts at one", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({ statusSequence: ["downloading"] });
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      yield* h.setState({ ...UPDATE_CONNECTED_STATE, phase: "backoff" });
      yield* drainAtoms;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toMatchObject({
        phase: "restarting",
      });

      yield* h.replaceSupervisor;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "succeeded",
        version: "0.5.0",
      });
      expect(h.installs.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      expect(h.followers.active).toBe(0);
    }),
  );

  it.effect("ends a blocked run with the supervisor's failure message", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({ statusSequence: ["downloading"] });
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      yield* h.setState({
        ...UPDATE_CONNECTED_STATE,
        phase: "blocked",
        lastFailure: new ConnectionBlockedError({ reason: "authentication", detail: "X" }),
      });
      yield* drainAtoms;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "failed",
        failure: { kind: "blocked", message: "X" },
      });
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBeUndefined();
      expect(h.followers.active).toBe(0);
    }),
  );

  it.effect("reads identity from the session config without requesting server.getConfig", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({ statusSequence: ["downloading"] });
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      yield* h.replaceSupervisor;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "succeeded",
        version: "0.5.0",
      });
      expect(h.configRequests.size).toBe(0);
    }),
  );

  it.effect("dismisses terminal runs and leaves active runs alone", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness();
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toBeNull();
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* drainAtoms;
      h.atoms.dismiss(h.atomRegistry, UPDATE_ENVIRONMENT_ID);
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({ phase: "starting" });
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "up-to-date",
      });
      h.atoms.dismiss(h.atomRegistry, UPDATE_ENVIRONMENT_ID);
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toBeNull();
    }),
  );

  it.effect("retries only in backoff and no more often than every five seconds", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({ statusSequence: ["downloading"] });
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      yield* h.setState({ ...UPDATE_CONNECTED_STATE, phase: "backoff" });
      yield* drainAtoms;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      yield* h.clock.adjust("4 seconds");
      yield* drainAtoms;
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBe(1);
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBe(2);
      yield* h.setState({ ...UPDATE_CONNECTED_STATE, phase: "connecting", stage: "opening" });
      yield* drainAtoms;
      yield* h.clock.adjust("10 seconds");
      yield* drainAtoms;
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBe(2);
    }),
  );

  it.effect("does not reconnect a user-disconnected host and ends as not back", () =>
    Effect.gen(function* () {
      const h = yield* makeUpdateHarness({ statusSequence: ["downloading"] });
      void h.atoms.update.run(h.atomRegistry, UPDATE_TARGET);
      yield* h.release(UPDATE_ENVIRONMENT_ID);
      yield* drainAtoms;
      yield* h.setState({ ...UPDATE_CONNECTED_STATE, phase: "available", desired: false });
      yield* drainAtoms;
      yield* h.clock.adjust("1 second");
      yield* drainAtoms;
      yield* h.clock.adjust("3 minutes");
      yield* drainAtoms;
      expect(h.atomRegistry.get(h.atoms.run(UPDATE_ENVIRONMENT_ID))).toEqual({
        phase: "failed",
        failure: { kind: "not-back" },
      });
      expect(h.retries.get(UPDATE_ENVIRONMENT_ID)).toBeUndefined();
      expect(h.followers.active).toBe(0);
    }),
  );
});

const advance = (milliseconds: number) =>
  Effect.sync(() => vi.advanceTimersByTime(milliseconds)).pipe(Effect.andThen(drainAtoms));

function settledState(result: AsyncResult.AsyncResult<RemoteUpdateSnapshot, unknown>) {
  return AsyncResult.isSuccess(result) && !result.waiting ? result.value.state : result._tag;
}

function snapshotSequence(states: ReadonlyArray<RemoteUpdateSnapshot["state"]>) {
  return (request: number) =>
    Effect.succeed(snapshotIn(states[Math.min(request, states.length) - 1]!));
}

describe("remote update status freshness", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it.effect("re-reads on the busy interval and stops once the host settles", () =>
    Effect.gen(function* () {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const h = yield* makeStatusHarness(
        snapshotSequence(["downloading", "installing", "up-to-date"]),
      );
      yield* drainAtoms;
      expect(h.requests.count).toBe(1);
      expect(h.state()).toBe("downloading");

      yield* advance(REMOTE_UPDATE_BUSY_REFRESH_MS - 1);
      expect(h.requests.count).toBe(1);

      yield* advance(1);
      expect(h.requests.count).toBe(2);
      expect(h.state()).toBe("installing");

      yield* advance(REMOTE_UPDATE_BUSY_REFRESH_MS);
      expect(h.requests.count).toBe(3);
      expect(h.state()).toBe("up-to-date");

      yield* advance(10 * REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS);
      expect(h.requests.count).toBe(3);
    }),
  );

  it.effect("re-reads a not-yet-checked host slowly until its own check lands", () =>
    Effect.gen(function* () {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const h = yield* makeStatusHarness(snapshotSequence(["idle", "idle", "up-to-date"]));
      yield* drainAtoms;
      expect(h.requests.count).toBe(1);

      yield* advance(REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS - 1);
      expect(h.requests.count).toBe(1);

      yield* advance(1);
      expect(h.requests.count).toBe(2);

      yield* advance(REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS);
      expect(h.requests.count).toBe(3);
      expect(h.state()).toBe("up-to-date");

      yield* advance(10 * REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS);
      expect(h.requests.count).toBe(3);
    }),
  );

  it.effect("stops once no view observes the status; only an already-armed read still runs", () =>
    Effect.gen(function* () {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const h = yield* makeStatusHarness(() => Effect.succeed(snapshotIn("downloading")));
      yield* drainAtoms;
      expect(h.requests.count).toBe(1);

      h.unmount();
      yield* advance(REMOTE_UPDATE_BUSY_REFRESH_MS);
      expect(h.requests.count).toBe(2);

      yield* advance(10 * REMOTE_UPDATE_BUSY_REFRESH_MS);
      expect(h.requests.count).toBe(2);
    }),
  );

  it.effect("never polls a manual host or a failed read", () =>
    Effect.gen(function* () {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const manual = yield* makeStatusHarness(() => Effect.succeed(snapshotIn("idle", "manual")));
      const failing = yield* makeStatusHarness(() =>
        Effect.fail(new TestUpdaterUnreachable({ message: "updater unreachable" })),
      );
      yield* drainAtoms;
      expect(manual.state()).toBe("idle");
      expect(failing.state()).toBe("Failure");

      yield* advance(10 * REMOTE_UPDATE_NOT_CHECKED_REFRESH_MS);
      expect(manual.requests.count).toBe(1);
      expect(failing.requests.count).toBe(1);
    }),
  );
});

describe("isRemoteUpdateUnchecked", () => {
  it("is true only for an automatic host that has not checked yet", () => {
    expect(isRemoteUpdateUnchecked(snapshotIn("idle", "interactive"))).toBe(true);
    expect(isRemoteUpdateUnchecked(snapshotIn("idle", "supervised"))).toBe(true);
    expect(isRemoteUpdateUnchecked(snapshotIn("idle", "manual"))).toBe(false);
    expect(isRemoteUpdateUnchecked(snapshotIn("up-to-date"))).toBe(false);
    expect(isRemoteUpdateUnchecked(null)).toBe(false);
  });
});

const LOST_SOCKET = new RpcClientError.RpcClientError({
  reason: new RpcClientError.RpcClientDefect({
    message: "socket closed",
    cause: new Error("socket closed"),
  }),
});
const NO_SESSION = new EnvironmentRpcUnavailableError({
  environmentId: "remote-update-status",
  message: "Remote update status test is not connected.",
});

describe("isRemoteUpdateConnectionFailure", () => {
  it("treats a lost, missing, or interrupted connection as no news about the updater", () => {
    expect(isRemoteUpdateConnectionFailure(Cause.fail(LOST_SOCKET))).toBe(true);
    expect(isRemoteUpdateConnectionFailure(Cause.fail(NO_SESSION))).toBe(true);
    expect(isRemoteUpdateConnectionFailure(Cause.interrupt())).toBe(true);
    expect(
      isRemoteUpdateConnectionFailure(Cause.combine(Cause.fail(NO_SESSION), Cause.interrupt())),
    ).toBe(true);
  });

  it("keeps failures that arrived over a live connection", () => {
    const updaterFailure = new TestUpdaterUnreachable({ message: "updater unreachable" });
    expect(isRemoteUpdateConnectionFailure(Cause.fail(updaterFailure))).toBe(false);
    expect(isRemoteUpdateConnectionFailure(Cause.fail(new Cause.TimeoutError()))).toBe(false);
    expect(isRemoteUpdateConnectionFailure(Cause.die(new Error("defect")))).toBe(false);
    expect(
      isRemoteUpdateConnectionFailure(
        Cause.combine(Cause.fail(NO_SESSION), Cause.fail(updaterFailure)),
      ),
    ).toBe(false);
  });
});

/**
 * A connected environment whose `updater.check` answers with `check(requestNumber)`,
 * with a view observing its check state, as the card and the settings rows do.
 */
const makeCheckHarness = Effect.fn("TestRemoteUpdates.makeCheckHarness")(function* (
  check: (
    request: number,
  ) => Effect.Effect<
    RemoteUpdateSnapshot,
    TestUpdaterUnreachable | RpcClientError.RpcClientError | EnvironmentRpcUnavailableError
  >,
) {
  const checks = { count: 0 };
  const session = {
    client: {
      [WS_METHODS.updaterStatus]: () => Effect.succeed(snapshotIn("idle", "manual")),
      [WS_METHODS.updaterCheck]: () =>
        Effect.suspend(() => {
          checks.count += 1;
          return check(checks.count);
        }),
    },
  } as unknown as RpcSession;
  const connection = yield* SubscriptionRef.make<{ phase: string; generation: number }>({
    phase: "connected",
    generation: 1,
  });
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: STATUS_ENVIRONMENT_ID, label: "Remote update check test" },
    session: yield* SubscriptionRef.make(Option.some(session)),
    state: connection,
  } as never);
  const installation = yield* SubscriptionRef.make(supervisor);
  const environment = EnvironmentRegistry.of({
    run: <A, E>(_: EnvironmentId, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
      SubscriptionRef.get(installation).pipe(
        Effect.flatMap((current) => Effect.provideService(effect, EnvironmentSupervisor, current)),
      ),
    followStream: (
      _: EnvironmentId,
      stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>,
    ) =>
      SubscriptionRef.changes(installation).pipe(
        Stream.switchMap((current) =>
          Stream.provideService(stream, EnvironmentSupervisor, current),
        ),
      ),
  } as never);
  const atoms = createRemoteUpdateEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environment)),
  );
  const registry = AtomRegistry.make();
  yield* Effect.addFinalizer(() => Effect.sync(() => registry.dispose()));
  const checkState = atoms.checkState(STATUS_ENVIRONMENT_ID);
  const unmount = registry.mount(checkState);
  yield* Effect.addFinalizer(() => Effect.sync(unmount));
  yield* drainAtoms;
  return {
    checks,
    connection,
    replaceSupervisor: Effect.gen(function* () {
      const replacement = EnvironmentSupervisor.of({
        target: supervisor.target,
        session: yield* SubscriptionRef.make(Option.some(session)),
        state: yield* SubscriptionRef.make({ phase: "connected", generation: 1 }),
      } as never);
      yield* SubscriptionRef.set(installation, replacement);
      yield* drainAtoms;
    }),
    state: () => registry.get(checkState),
    run: () =>
      Effect.promise(() => atoms.check.run(registry, STATUS_TARGET)).pipe(
        Effect.tap(() => drainAtoms),
      ),
  };
});

describe("remote update check state", () => {
  it.effect("records a check that failed over a live connection until a check succeeds", () =>
    Effect.gen(function* () {
      const h = yield* makeCheckHarness((request) =>
        request === 1
          ? Effect.fail(new TestUpdaterUnreachable({ message: "feed timed out" }))
          : Effect.succeed(snapshotIn("up-to-date")),
      );
      expect(h.state()).toEqual({ inFlight: false, failure: null });

      const failed = yield* h.run();
      expect(failed._tag).toBe("Failure");
      const failure = h.state().failure;
      expect(failure?.generation).toBe(1);
      expect(failure === null ? null : Cause.squash(failure.cause)).toMatchObject({
        message: "feed timed out",
      });

      const succeeded = yield* h.run();
      expect(succeeded._tag).toBe("Success");
      expect(h.state().failure).toBeNull();
    }),
  );

  it.effect("never shows a failure while disconnected, and forgets it after a reconnect", () =>
    Effect.gen(function* () {
      const h = yield* makeCheckHarness(() =>
        Effect.fail(new TestUpdaterUnreachable({ message: "feed timed out" })),
      );
      yield* h.run();
      expect(h.state().failure).not.toBeNull();

      yield* SubscriptionRef.set(h.connection, { phase: "backoff", generation: 1 });
      yield* drainAtoms;
      expect(h.state().failure).toBeNull();

      yield* SubscriptionRef.set(h.connection, { phase: "connected", generation: 2 });
      yield* drainAtoms;
      expect(h.state().failure).toBeNull();
    }),
  );

  it.effect("records nothing for a check that only lost its connection", () =>
    Effect.gen(function* () {
      const h = yield* makeCheckHarness((request) =>
        Effect.fail(request === 1 ? LOST_SOCKET : NO_SESSION),
      );
      expect((yield* h.run())._tag).toBe("Failure");
      expect((yield* h.run())._tag).toBe("Failure");
      expect(h.checks.count).toBe(2);
      expect(h.state().failure).toBeNull();
    }),
  );

  it.effect("forgets a failed check when a replacement supervisor reuses its generation", () =>
    Effect.gen(function* () {
      const h = yield* makeCheckHarness(() =>
        Effect.fail(new TestUpdaterUnreachable({ message: "feed timed out" })),
      );
      yield* h.run();
      expect(h.state().failure?.generation).toBe(1);

      yield* h.replaceSupervisor;
      expect(h.state()).toEqual({ inFlight: false, failure: null });

      // A failure from the replacement's own connection still reaches every view.
      yield* h.run();
      expect(h.state().failure?.generation).toBe(1);
    }),
  );

  it.effect("reports a check in flight to every view while it runs", () =>
    Effect.gen(function* () {
      const release = yield* Deferred.make<void>();
      const h = yield* makeCheckHarness(() =>
        Deferred.await(release).pipe(Effect.as(snapshotIn("up-to-date"))),
      );
      const running = yield* h.run().pipe(Effect.forkChild({ startImmediately: true }));
      yield* drainAtoms;
      expect(h.state().inFlight).toBe(true);

      yield* Deferred.succeed(release, undefined);
      yield* Fiber.join(running);
      expect(h.state()).toEqual({ inFlight: false, failure: null });
    }),
  );
});
