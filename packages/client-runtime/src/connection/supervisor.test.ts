import { EnvironmentId, type ServerConfig } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Clock from "effect/Clock";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Random from "effect/Random";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";

import * as Persistence from "../platform/persistence.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { ConnectionCatalogEntry } from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionDriver from "./driver.ts";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  type ConnectionAttemptError,
  type ConnectionTarget,
  type NetworkStatus,
  type PreparedConnection,
  type SupervisorConnectionState,
} from "./model.ts";
import * as RpcSession from "../rpc/session.ts";
import * as ConnectionResolver from "./resolver.ts";
import { EnvironmentSelection } from "./selection.ts";
import * as EnvironmentSupervisor from "./supervisor.ts";
import * as ConnectionWakeups from "./wakeups.ts";

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});

const RELAY_TARGET = new RelayConnectionTarget({
  environmentId: TARGET.environmentId,
  label: TARGET.label,
});

const TARGET_ENTRY: ConnectionCatalogEntry = {
  target: TARGET,
  profile: Option.none(),
};

const RELAY_ENTRY: ConnectionCatalogEntry = {
  target: RELAY_TARGET,
  profile: Option.none(),
};

const makeSupervisor = Effect.fn("TestConnectionHarness.makeSupervisor")(function* (
  entry: ConnectionCatalogEntry,
  options: Pick<EnvironmentSupervisor.EnvironmentSupervisorOptions, "initiallyDesired">,
) {
  const targetRef = yield* Ref.make(entry.target);
  return yield* EnvironmentSupervisor.make(entry, {
    initiallyDesired: options.initiallyDesired,
    targetRef,
  });
});

const PREPARED_CONNECTION: PreparedConnection = {
  environmentId: TARGET.environmentId,
  label: TARGET.label,
  descriptor: {
    environmentId: TARGET.environmentId,
    label: TARGET.label,
    platform: { os: "linux", arch: "x64" },
    serverVersion: "0.0.0-test",
    storageInstanceId: "store-test",
    bootId: null,
    remoteUpdateSupport: null,
    remoteProtocolVersion: 1,
    minCompatibleRemoteProtocol: 1,
    capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
  },
  httpBaseUrl: TARGET.httpBaseUrl,
  socketUrl: "wss://environment.example.test/ws",
  httpAuthorization: null,
  e2ee: null,
  target: TARGET,
};

const TEST_RPC_CLIENT = {} as WsRpcProtocolClient;

function transient(message = "Connection failed.") {
  return new ConnectionTransientError({
    reason: "transport",
    detail: message,
  });
}

function blocked(message = "Authentication required.") {
  return new ConnectionBlockedError({
    reason: "authentication",
    detail: message,
  });
}

function awaitState(
  state: SubscriptionRef.SubscriptionRef<SupervisorConnectionState>,
  predicate: (value: SupervisorConnectionState) => boolean,
) {
  return SubscriptionRef.changes(state).pipe(
    Stream.filter(predicate),
    Stream.runHead,
    Effect.map(Option.getOrThrow),
  );
}

const eventuallyState = Effect.fn("TestConnectionHarness.eventuallyState")(function* (
  state: SubscriptionRef.SubscriptionRef<SupervisorConnectionState>,
  predicate: (value: SupervisorConnectionState) => boolean,
) {
  let lastState = yield* SubscriptionRef.get(state);
  for (let iteration = 0; iteration < 100; iteration += 1) {
    lastState = yield* SubscriptionRef.get(state);
    if (predicate(lastState)) {
      return lastState;
    }
    yield* Effect.yieldNow;
  }
  return yield* Effect.die(
    new Error(
      `Expected supervisor state was not observed. Last state: phase=${lastState.phase}, stage=${lastState.stage ?? "none"}, attempt=${lastState.attempt}, generation=${lastState.generation}`,
    ),
  );
});

const makeHarness = Effect.fn("TestConnectionHarness.make")(function* (options?: {
  readonly networkStatus?: NetworkStatus;
  readonly prepare?: (
    attempt: number,
    target: ConnectionTarget,
  ) => Effect.Effect<PreparedConnection, ConnectionAttemptError>;
  readonly ready?: (attempt: number) => Effect.Effect<void, ConnectionAttemptError>;
  readonly connected?: (attempt: number) => Effect.Effect<void, ConnectionAttemptError>;
  readonly afterSynchronizing?: Effect.Effect<void>;
  readonly probe?: (attempt: number) => Effect.Effect<void, ConnectionAttemptError>;
  readonly random?: number;
  readonly selection?: SubscriptionRef.SubscriptionRef<EnvironmentId | null>;
}) {
  const selection = options?.selection ?? (yield* SubscriptionRef.make<EnvironmentId | null>(null));
  const networkStatus = yield* SubscriptionRef.make<NetworkStatus>(
    options?.networkStatus ?? "online",
  );
  const prepareCount = yield* Ref.make(0);
  const sessionCount = yield* Ref.make(0);
  const releaseCount = yield* Ref.make(0);
  const wakeups = yield* SubscriptionRef.make<{
    readonly sequence: number;
    readonly reason: "application-active" | "credentials-changed";
  }>({
    sequence: 0,
    reason: "application-active",
  });
  const focusWakeups = yield* SubscriptionRef.make(0);
  const closedSessions = yield* Ref.make<
    ReadonlyArray<Deferred.Deferred<never, ConnectionTransientError>>
  >([]);

  const connectivity = Connectivity.Connectivity.of({
    status: SubscriptionRef.get(networkStatus),
    changes: SubscriptionRef.changes(networkStatus),
  });

  const prepare = Effect.fn("TestConnectionDriver.prepare")(function* (target: ConnectionTarget) {
    const attempt = yield* Ref.updateAndGet(prepareCount, (count) => count + 1);
    if (options?.prepare) {
      return yield* options.prepare(attempt, target);
    }
    return PREPARED_CONNECTION;
  });

  const connect = Effect.fn("TestConnectionDriver.connect")(function* (
    entry: ConnectionCatalogEntry,
    reportProgress: (progress: ConnectionDriver.ConnectionDriverProgress) => Effect.Effect<void>,
  ) {
    const target = entry.target;
    yield* reportProgress({ stage: "preparing" });
    const prepared = yield* prepare(target);
    yield* reportProgress({ stage: "opening", prepared });

    const attempt = yield* Ref.updateAndGet(sessionCount, (count) => count + 1);
    const closed = yield* Deferred.make<never, ConnectionTransientError>();
    yield* Ref.update(closedSessions, (sessions) => [...sessions, closed]);

    const session = yield* Effect.acquireRelease(
      Effect.succeed({
        client: TEST_RPC_CLIENT,
        initialConfig: Effect.die(new Error("Initial config is not used by supervisor tests.")),
        ready: options?.ready?.(attempt) ?? Effect.void,
        probe: options?.probe?.(attempt) ?? Effect.void,
        closed: Deferred.await(closed),
        e2eeAuthenticated: Effect.succeed(null),
      } satisfies RpcSession.RpcSession),
      () => Ref.update(releaseCount, (count) => count + 1),
    );

    yield* options?.connected?.(attempt) ?? Effect.void;
    yield* reportProgress({ stage: "configuring", prepared });
    yield* session.ready;
    yield* reportProgress({ stage: "synchronizing", prepared });
    yield* options?.afterSynchronizing ?? Effect.void;
    return {
      prepared,
      session,
      open: Effect.void,
    } satisfies ConnectionDriver.EnvironmentConnectionLease;
  });

  const dependencies = Layer.mergeAll(
    Layer.succeed(Connectivity.Connectivity, connectivity),
    Layer.succeed(
      ConnectionWakeups.ConnectionWakeups,
      ConnectionWakeups.ConnectionWakeups.of({
        focusVisibility: SubscriptionRef.changes(focusWakeups).pipe(
          Stream.drop(1),
          Stream.map(() => undefined),
        ),
        changes: SubscriptionRef.changes(wakeups).pipe(
          Stream.drop(1),
          Stream.map((event) => event.reason),
        ),
      }),
    ),
    Layer.succeed(
      ConnectionDriver.ConnectionDriver,
      ConnectionDriver.ConnectionDriver.of({ connect }),
    ),
    Layer.succeed(Random.Random, {
      nextDoubleUnsafe: () => options?.random ?? 0.5,
      nextIntUnsafe: () => 0,
    }),
    Layer.succeed(
      EnvironmentSelection,
      EnvironmentSelection.of({
        current: SubscriptionRef.get(selection),
        changes: SubscriptionRef.changes(selection),
      }),
    ),
  );

  return {
    dependencies,
    prepareCount,
    focus: SubscriptionRef.update(focusWakeups, (count) => count + 1),
    sessionCount,
    releaseCount,
    setNetworkStatus: (status: NetworkStatus) => SubscriptionRef.set(networkStatus, status),
    wake: (reason: "application-active" | "credentials-changed") =>
      SubscriptionRef.update(wakeups, (event) => ({
        sequence: event.sequence + 1,
        reason,
      })),
    closeLatestSession: Effect.fn("TestConnectionHarness.closeLatestSession")(function* (
      error = transient("Session closed."),
    ) {
      const sessions = yield* Ref.get(closedSessions);
      const latest = sessions.at(-1);
      if (latest) {
        yield* Deferred.fail(latest, error);
      }
    }),
  };
});

const makeStorageIdentityHarness = Effect.fn("TestStorageIdentityHarness.make")(function* (
  acceptedStorageInstanceId: string | null,
  reportedStorageInstanceId: string | null,
  options?: {
    readonly beforeInitialConfig?: Effect.Effect<void>;
    readonly failAcceptance?: boolean;
    readonly sessionReportedStorageInstanceId?: string | null;
  },
) {
  const acceptedIdentities = yield* Ref.make<Map<string, string>>(
    new Map(
      acceptedStorageInstanceId === null
        ? []
        : [["platform:primary", acceptedStorageInstanceId] as const],
    ),
  );
  const sessionConnectCount = yield* Ref.make(0);
  const sessionReleaseCount = yield* Ref.make(0);
  const identityAcceptCount = yield* Ref.make(0);
  const acceptedAtSessionConnect = yield* Ref.make<string | null>(null);
  const prepared: PreparedConnection = {
    ...PREPARED_CONNECTION,
    descriptor: {
      ...PREPARED_CONNECTION.descriptor,
      storageInstanceId: reportedStorageInstanceId,
    },
  };
  const resolver = ConnectionResolver.ConnectionResolver.of({
    prepare: () => Effect.succeed(prepared),
  });
  const sessions = RpcSession.RpcSessionFactory.of({
    connect: () =>
      Ref.get(acceptedIdentities).pipe(
        Effect.tap((current) =>
          Ref.set(acceptedAtSessionConnect, current.get("platform:primary") ?? null),
        ),
        Effect.andThen(Ref.update(sessionConnectCount, (count) => count + 1)),
        Effect.andThen(
          Effect.acquireRelease(
            Effect.succeed({
              client: TEST_RPC_CLIENT,
              initialConfig: (options?.beforeInitialConfig ?? Effect.void).pipe(
                Effect.as({
                  environment: {
                    ...prepared.descriptor,
                    storageInstanceId:
                      options?.sessionReportedStorageInstanceId === undefined
                        ? reportedStorageInstanceId
                        : options.sessionReportedStorageInstanceId,
                  },
                } as ServerConfig),
              ),
              ready: Effect.void,
              connected: Effect.void,
              probe: Effect.void,
              closed: Effect.never,
              e2eeAuthenticated: Effect.succeed(null),
            } satisfies RpcSession.EstablishingRpcSession),
            () => Ref.update(sessionReleaseCount, (count) => count + 1),
          ),
        ),
      ),
  });
  const identityService = {
    get: (targetKey: string) =>
      Ref.get(acceptedIdentities).pipe(
        Effect.map((current) => Option.fromUndefinedOr(current.get(targetKey))),
      ),
    accept: ({ targetKey, storageInstanceId }: Persistence.AcceptedStorageIdentity) =>
      options?.failAcceptance === true
        ? Effect.fail(
            new Persistence.ConnectionPersistenceError({
              operation: "accept-storage-identity",
              message: "Storage is unavailable.",
            }),
          )
        : Ref.update(acceptedIdentities, (current) => {
            const next = new Map(current);
            next.set(targetKey, storageInstanceId);
            return next;
          }).pipe(Effect.andThen(Ref.update(identityAcceptCount, (count) => count + 1))),
    rollbackAcceptance: (
      { targetKey, storageInstanceId }: Persistence.AcceptedStorageIdentity,
      previousStorageInstanceId: string | null,
    ) =>
      options?.failAcceptance === true
        ? Effect.fail(
            new Persistence.ConnectionPersistenceError({
              operation: "accept-storage-identity",
              message: "Storage is unavailable.",
            }),
          )
        : Ref.modify(acceptedIdentities, (current) => {
            if (current.get(targetKey) !== storageInstanceId) {
              return [false, current] as const;
            }
            const next = new Map(current);
            if (previousStorageInstanceId === null) next.delete(targetKey);
            else next.set(targetKey, previousStorageInstanceId);
            return [true, next] as const;
          }),
    transition: <A>(
      targetKey: string,
      decide: (
        acceptedStorageInstanceId: string | null,
      ) => Persistence.AcceptedStorageIdentityTransition<A>,
    ) =>
      Ref.get(acceptedIdentities).pipe(
        Effect.flatMap((current) => {
          const transition = decide(current.get(targetKey) ?? null);
          if (transition.mutation._tag === "Keep") {
            return Effect.succeed(transition.result);
          }
          if (options?.failAcceptance === true) {
            return Effect.fail(
              new Persistence.ConnectionPersistenceError({
                operation: "accept-storage-identity",
                message: "Storage is unavailable.",
              }),
            );
          }
          const next = new Map(current);
          next.set(targetKey, transition.mutation.storageInstanceId);
          return Ref.set(acceptedIdentities, next).pipe(
            Effect.andThen(Ref.update(identityAcceptCount, (count) => count + 1)),
            Effect.as(transition.result),
          );
        }),
      ),
  };
  const identities = Persistence.AcceptedStorageIdentityStore.of(identityService);
  const driver = yield* ConnectionDriver.make.pipe(
    Effect.provideService(ConnectionResolver.ConnectionResolver, resolver),
    Effect.provideService(RpcSession.RpcSessionFactory, sessions),
    Effect.provideService(Persistence.AcceptedStorageIdentityStore, identities),
  );
  const networkStatus = yield* SubscriptionRef.make<NetworkStatus>("online");
  const dependencies = Layer.mergeAll(
    Layer.succeed(
      Connectivity.Connectivity,
      Connectivity.Connectivity.of({
        status: SubscriptionRef.get(networkStatus),
        changes: SubscriptionRef.changes(networkStatus),
      }),
    ),
    Layer.succeed(
      ConnectionWakeups.ConnectionWakeups,
      ConnectionWakeups.ConnectionWakeups.of({
        focusVisibility: Stream.never,
        changes: Stream.never,
      }),
    ),
    Layer.succeed(ConnectionDriver.ConnectionDriver, driver),
    Layer.succeed(Random.Random, { nextDoubleUnsafe: () => 0.5, nextIntUnsafe: () => 0 }),
  );

  return {
    acceptedAtSessionConnect,
    acceptedIdentities,
    dependencies,
    identityAcceptCount,
    sessionConnectCount,
    sessionReleaseCount,
  };
});

describe("EnvironmentSupervisor", () => {
  it.effect("blocks a changed store before opening or synchronizing the RPC session", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness("store-a", "store-b");
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "blocked" || current.phase === "connected",
      );

      expect(state).toMatchObject({
        phase: "blocked",
        lastFailure: {
          _tag: "ConnectionStorageChangedError",
          reason: "storage-changed",
          targetKey: "platform:primary",
          acceptedStorageInstanceId: "store-a",
          reportedStorageInstanceId: "store-b",
        },
      });
      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(0);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
    }),
  );

  it.effect("persists the first reported store before opening the RPC session", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness(null, "store-a");
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* eventuallyState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.acceptedAtSessionConnect)).toBe("store-a");
      expect(yield* Ref.get(harness.acceptedIdentities)).toEqual(
        new Map([["platform:primary", "store-a"]]),
      );
      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(1);
      expect(yield* Ref.get(harness.identityAcceptCount)).toBe(1);
    }),
  );

  it.effect("opens one RPC session when the reported store matches the accepted store", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness("store-a", "store-a");
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* eventuallyState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(1);
      expect(yield* Ref.get(harness.identityAcceptCount)).toBe(0);
    }),
  );

  it.effect("publishes each connection's session once, from configuring on", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness("store-a", "store-a");
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: false,
      }).pipe(Effect.provide(harness.dependencies));
      const published = yield* Ref.make(0);
      const early = yield* Ref.make<ReadonlyArray<unknown>>([]);
      yield* SubscriptionRef.changes(supervisor.session).pipe(
        Stream.runForEach((session) =>
          Option.isSome(session) ? Ref.update(published, (count) => count + 1) : Effect.void,
        ),
        Effect.forkChild,
      );
      yield* SubscriptionRef.changes(supervisor.subscriptionSession!).pipe(
        Stream.runForEach((session) =>
          Option.isSome(session)
            ? Ref.update(early, (current) => [...current, session.value])
            : Effect.void,
        ),
        Effect.forkChild,
      );
      for (let iteration = 0; iteration < 20; iteration += 1) yield* Effect.yieldNow;
      yield* supervisor.connect;
      yield* eventuallyState(supervisor.state, (state) => state.phase === "connected");
      for (let iteration = 0; iteration < 50; iteration += 1) yield* Effect.yieldNow;
      // A second publication of the same session makes every consumer subscribe again.
      expect(yield* Ref.get(published)).toBe(1);
      expect(yield* Ref.get(early)).toHaveLength(1);
      expect((yield* Ref.get(early))[0]).toBe(
        Option.getOrNull(yield* SubscriptionRef.get(supervisor.session)),
      );
    }),
  );

  it.effect("blocks a session config store change without publishing or retrying the lease", () =>
    Effect.gen(function* () {
      const initialConfigStarted = yield* Deferred.make<void>();
      const releaseInitialConfig = yield* Deferred.make<void>();
      const harness = yield* makeStorageIdentityHarness("store-a", "store-a", {
        beforeInitialConfig: Deferred.succeed(initialConfigStarted, undefined).pipe(
          Effect.andThen(Deferred.await(releaseInitialConfig)),
        ),
        sessionReportedStorageInstanceId: "store-b",
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* Deferred.await(initialConfigStarted);
      expect((yield* SubscriptionRef.get(supervisor.state)).stage).toBe("configuring");
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.session))).toBe(true);
      // Read subscriptions already follow the gated session; it releases nothing unverified.
      expect(Option.isSome(yield* SubscriptionRef.get(supervisor.subscriptionSession!))).toBe(true);
      yield* Deferred.succeed(releaseInitialConfig, undefined);

      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "blocked",
      );
      for (let iteration = 0; iteration < 100; iteration += 1) {
        yield* Effect.yieldNow;
      }

      expect(state).toMatchObject({
        phase: "blocked",
        lastFailure: {
          _tag: "ConnectionStorageChangedError",
          acceptedStorageInstanceId: "store-a",
          reportedStorageInstanceId: "store-b",
        },
      });
      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(1);
      expect(yield* Ref.get(harness.sessionReleaseCount)).toBe(1);
      expect(yield* Ref.get(harness.identityAcceptCount)).toBe(0);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.session))).toBe(true);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.subscriptionSession!))).toBe(true);
      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("blocked");
    }),
  );

  it.effect("blocks before opening when the first store identity cannot be persisted", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness(null, "store-a", {
        failAcceptance: true,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "blocked" || current.phase === "connected",
      );

      expect(state).toMatchObject({
        phase: "blocked",
        lastFailure: {
          _tag: "ConnectionBlockedError",
          reason: "configuration",
        },
      });
      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(0);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
    }),
  );

  it.effect("keeps an accepted store when an older server reports no storage identity", () =>
    Effect.gen(function* () {
      const harness = yield* makeStorageIdentityHarness("store-a", null);
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* eventuallyState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.acceptedIdentities)).toEqual(
        new Map([["platform:primary", "store-a"]]),
      );
      expect(yield* Ref.get(harness.identityAcceptCount)).toBe(0);
      expect(yield* Ref.get(harness.sessionConnectCount)).toBe(1);
    }),
  );

  it.effect("does not attempt a connection until it is desired", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: false,
      }).pipe(Effect.provide(harness.dependencies));

      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("available");
      expect(yield* Ref.get(harness.prepareCount)).toBe(0);
    }),
  );

  it.effect("does not let the initial connect signal cancel the first attempt", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: false,
      }).pipe(Effect.provide(harness.dependencies));

      yield* supervisor.connect;
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.sessionCount)).toBe(1);
      expect(yield* Ref.get(harness.releaseCount)).toBe(0);
    }),
  );

  it.effect("waits while offline and connects immediately when the network returns", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({ networkStatus: "offline" });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "offline");
      expect(yield* Ref.get(harness.prepareCount)).toBe(0);

      yield* harness.setNetworkStatus("online");
      const ready = yield* awaitState(supervisor.state, (state) => state.phase === "connected");

      expect(ready).toMatchObject({
        desired: true,
        network: "online",
        phase: "connected",
        attempt: 1,
        generation: 1,
        lastFailure: null,
      });
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);
    }),
  );

  it.effect("retries forever with exponential backoff capped at sixteen seconds", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: () => Effect.fail(transient()),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);

      for (const [index, delay] of [1_000, 2_000, 4_000, 8_000, 16_000, 16_000].entries()) {
        yield* TestClock.adjust(delay);
        yield* eventuallyState(
          supervisor.state,
          (state) => state.phase === "backoff" && state.attempt === index + 2,
        );
      }

      expect(yield* Ref.get(harness.prepareCount)).toBe(7);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("keeps the latest failure visible throughout the next connection attempt", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1 ? Effect.fail(transient("Relay connection timed out.")) : Effect.never,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      yield* TestClock.adjust("1 second");

      const retrying = yield* awaitState(
        supervisor.state,
        (state) =>
          state.phase === "connecting" && state.stage === "preparing" && state.attempt === 2,
      );
      expect(retrying).toMatchObject({
        phase: "connecting",
        stage: "preparing",
        attempt: 2,
        lastFailure: {
          _tag: "ConnectionTransientError",
          reason: "transport",
          message: "Relay connection timed out.",
        },
      });
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("retries at the configuring ceiling when a session never becomes ready", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        ready: () => Effect.never,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connecting" && state.stage === "configuring",
      );
      yield* TestClock.adjust("119 seconds");
      expect(yield* SubscriptionRef.get(supervisor.state)).toMatchObject({
        phase: "connecting",
        stage: "configuring",
        attempt: 1,
      });

      yield* TestClock.adjust("1 second");
      const failedAt = yield* Clock.currentTimeMillis;
      const retrying = yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "backoff",
      );

      expect(retrying).toMatchObject({
        phase: "backoff",
        attempt: 1,
        retryAt: failedAt + 1000,
        lastFailure: {
          _tag: "ConnectionTransientError",
          reason: "timeout",
          message: "Test environment took more than 2 minutes to send its settings.",
        },
      });
      expect(retrying).not.toHaveProperty("notice");
      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
      yield* TestClock.adjust("1 second");
      const next = yield* eventuallyState(
        supervisor.state,
        (state) => state.stage === "configuring" && state.attempt === 2,
      );
      expect(next).not.toHaveProperty("notice");
      expect(yield* Ref.get(harness.sessionCount)).toBe(2);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("times out an unopened socket at fifteen seconds and releases the session", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({ connected: () => Effect.never });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
        Effect.provide(harness.dependencies),
      );
      yield* awaitState(supervisor.state, (state) => state.stage === "opening");
      yield* TestClock.adjust("14 seconds");
      expect(yield* SubscriptionRef.get(supervisor.state)).toMatchObject({
        phase: "connecting",
        stage: "opening",
        attempt: 1,
      });
      yield* TestClock.adjust("1 second");
      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "backoff",
      );
      expect(state).toMatchObject({
        phase: "backoff",
        attempt: 1,
        lastFailure: {
          reason: "timeout",
          message: "Test environment did not respond during connection setup.",
        },
      });
      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
    }),
  );

  it.effect("connects on the first attempt when settings take forty seconds", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({ ready: () => Effect.sleep("40 seconds") });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
        Effect.provide(harness.dependencies),
      );
      yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
      yield* TestClock.adjust("40 seconds");
      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "connected",
      );
      expect(state).toMatchObject({ phase: "connected", attempt: 1, lastFailure: null });
      expect(state).not.toHaveProperty("notice");
      expect(yield* Ref.get(harness.sessionCount)).toBe(1);
      expect(yield* Ref.get(harness.releaseCount)).toBe(0);
      yield* TestClock.adjust("120 seconds");
      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("connected");
      expect(yield* Ref.get(harness.releaseCount)).toBe(0);
    }),
  );

  it.effect("starts the settings notice and ceiling when the socket connects", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        connected: () => Effect.sleep("14 seconds"),
        ready: () => Effect.never,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
        Effect.provide(harness.dependencies),
      );
      yield* awaitState(supervisor.state, (state) => state.stage === "opening");
      yield* TestClock.adjust("14 seconds");
      yield* eventuallyState(supervisor.state, (state) => state.stage === "configuring");
      yield* TestClock.adjust("4 seconds");
      expect(yield* SubscriptionRef.get(supervisor.state)).toMatchObject({
        stage: "configuring",
        attempt: 1,
      });
      expect(yield* SubscriptionRef.get(supervisor.state)).not.toHaveProperty("notice");
      yield* TestClock.adjust("1 second");
      expect((yield* SubscriptionRef.get(supervisor.state)).notice).toBe(
        "Receiving settings from Test environment over a slow connection…",
      );
      yield* TestClock.adjust("114 seconds");
      expect(yield* SubscriptionRef.get(supervisor.state)).toMatchObject({
        stage: "configuring",
        attempt: 1,
      });
      yield* TestClock.adjust("1 second");
      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "backoff",
      );
      expect(state.lastFailure?.message).toBe(
        "Test environment took more than 2 minutes to send its settings.",
      );
    }),
  );

  it.effect("uses the current saved label for slow settings notices and ceiling failures", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({ ready: () => Effect.never });
      const targetRef = yield* Ref.make<ConnectionTarget>(TARGET);
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        initiallyDesired: true,
        targetRef,
      }).pipe(Effect.provide(harness.dependencies));
      yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
      yield* Ref.set(targetRef, new PrimaryConnectionTarget({ ...TARGET, label: "GPU box" }));
      yield* TestClock.adjust("5 seconds");
      expect((yield* SubscriptionRef.get(supervisor.state)).notice).toBe(
        "Receiving settings from GPU box over a slow connection…",
      );
      yield* Ref.set(targetRef, new PrimaryConnectionTarget({ ...TARGET, label: "Build server" }));
      yield* TestClock.adjust("115 seconds");
      const state = yield* eventuallyState(
        supervisor.state,
        (current) => current.phase === "backoff",
      );
      expect(state.lastFailure?.message).toBe(
        "Build server took more than 2 minutes to send its settings.",
      );
    }),
  );

  it.effect(
    "shows a slow settings notice after five seconds and clears it on synchronization",
    () =>
      Effect.gen(function* () {
        const ready = yield* Deferred.make<void>();
        const synchronized = yield* Deferred.make<void>();
        const harness = yield* makeHarness({
          ready: () => Deferred.await(ready),
          afterSynchronizing: Deferred.await(synchronized),
        });
        const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
          Effect.provide(harness.dependencies),
        );
        yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
        yield* TestClock.adjust("4 seconds");
        expect(yield* SubscriptionRef.get(supervisor.state)).not.toHaveProperty("notice");
        yield* TestClock.adjust("1 second");
        expect(yield* SubscriptionRef.get(supervisor.state)).toMatchObject({
          stage: "configuring",
          notice: "Receiving settings from Test environment over a slow connection…",
        });
        yield* Deferred.succeed(ready, undefined);
        const synchronizing = yield* eventuallyState(
          supervisor.state,
          (state) => state.stage === "synchronizing",
        );
        expect(synchronizing).not.toHaveProperty("notice");
        yield* Deferred.succeed(synchronized, undefined);
        const connected = yield* eventuallyState(
          supervisor.state,
          (state) => state.phase === "connected",
        );
        expect(connected).not.toHaveProperty("notice");
      }),
  );

  it.effect("does not overwrite synchronization with a delayed slow settings notice", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        ready: () => Effect.sleep("4 seconds"),
        afterSynchronizing: Effect.never,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
        Effect.provide(harness.dependencies),
      );
      yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
      yield* TestClock.adjust("4 seconds");
      const synchronizing = yield* eventuallyState(
        supervisor.state,
        (state) => state.stage === "synchronizing",
      );
      expect(synchronizing).not.toHaveProperty("notice");
      yield* TestClock.adjust("1 second");
      expect(yield* SubscriptionRef.get(supervisor.state)).toEqual(synchronizing);
      yield* TestClock.adjust("20 seconds");
      expect(yield* SubscriptionRef.get(supervisor.state)).toEqual(synchronizing);
    }),
  );

  it.effect("reports liveness silence during configuring with the environment label", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        ready: () =>
          Effect.sleep("30 seconds").pipe(
            Effect.andThen(
              Effect.fail(
                new ConnectionTransientError({
                  reason: "liveness-timeout",
                  detail: "The connection disconnected.",
                }),
              ),
            ),
          ),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
        Effect.provide(harness.dependencies),
      );
      yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
      yield* TestClock.adjust("30 seconds");
      const state = yield* eventuallyState(
        supervisor.state,
        (current) =>
          current.phase === "backoff" && current.lastFailure?.reason === "liveness-timeout",
      );
      expect(state).toMatchObject({
        attempt: 1,
        lastFailure: {
          reason: "liveness-timeout",
          message:
            "No data from Test environment for 30 seconds. The connection is too slow or was lost.",
        },
      });
      expect(state).not.toHaveProperty("notice");
      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
    }),
  );

  for (const action of ["disconnect", "retry", "offline"] as const) {
    it.effect(`interrupts configuring and clears its notice on ${action}`, () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness({
          ready: (attempt) => (attempt === 1 ? Effect.never : Effect.void),
        });
        const supervisor = yield* makeSupervisor(TARGET_ENTRY, { initiallyDesired: true }).pipe(
          Effect.provide(harness.dependencies),
        );
        yield* awaitState(supervisor.state, (state) => state.stage === "configuring");
        yield* TestClock.adjust("5 seconds");
        yield* action === "disconnect"
          ? supervisor.disconnect
          : action === "retry"
            ? supervisor.retryNow
            : harness.setNetworkStatus("offline");
        const phase =
          action === "disconnect" ? "available" : action === "retry" ? "connected" : "offline";
        const state = yield* eventuallyState(
          supervisor.state,
          (current) => current.phase === phase,
        );
        expect(state).not.toHaveProperty("notice");
        expect(state.lastFailure).toBeNull();
        expect(yield* Ref.get(harness.releaseCount)).toBe(1);
        expect(yield* Ref.get(harness.sessionCount)).toBe(action === "retry" ? 2 : 1);
        yield* TestClock.adjust("120 seconds");
        expect(yield* SubscriptionRef.get(supervisor.state)).toEqual(state);
        expect(yield* Ref.get(harness.releaseCount)).toBe(1);
      }),
    );
  }

  it.effect("interrupts and releases a connection attempt when setup times out", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: () => Effect.never,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connecting" && state.stage === "preparing",
      );
      yield* TestClock.adjust("15 seconds");
      const retrying = yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );

      expect(retrying).toMatchObject({
        lastFailure: {
          _tag: "ConnectionTransientError",
          reason: "timeout",
          message: "Test environment did not respond during connection setup.",
        },
      });
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("converts unexpected driver defects into retryable failures", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1
            ? Effect.die(new Error("Native transport defect."))
            : Effect.succeed(PREPARED_CONNECTION),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      const failed = yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      expect(failed).toMatchObject({
        lastFailure: {
          _tag: "ConnectionTransientError",
          reason: "transport",
          message: "Test environment connection failed unexpectedly.",
        },
      });

      yield* TestClock.adjust("1 second");
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("explicit retry interrupts the current backoff", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1 ? Effect.fail(transient()) : Effect.succeed(PREPARED_CONNECTION),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "backoff");
      yield* supervisor.retryNow;
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
    }),
  );

  it.effect("keeps blocked failures idle until an external signal requests another attempt", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1 ? Effect.fail(blocked()) : Effect.succeed(PREPARED_CONNECTION),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "blocked");
      yield* TestClock.adjust("1 hour");
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);

      yield* supervisor.retryNow;
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("releases a live session while offline and starts a new generation when online", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 1,
      );
      yield* harness.setNetworkStatus("offline");
      yield* awaitState(supervisor.state, (state) => state.phase === "offline");

      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.session))).toBe(true);

      yield* harness.setNetworkStatus("online");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );
      expect(yield* Ref.get(harness.sessionCount)).toBe(2);
    }),
  );

  it.effect("retries a blocked connection when platform credentials change", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1 ? Effect.fail(blocked()) : Effect.succeed(PREPARED_CONNECTION),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "blocked");
      yield* harness.wake("credentials-changed");
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
    }),
  );

  it.effect("does not let platform wakeups reset an in-flight attempt", () =>
    Effect.gen(function* () {
      const firstAttemptStarted = yield* Deferred.make<void>();
      const harness = yield* makeHarness({
        prepare: () =>
          Deferred.succeed(firstAttemptStarted, undefined).pipe(Effect.andThen(Effect.never)),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* Deferred.await(firstAttemptStarted);
      yield* Effect.all(
        [
          harness.wake("credentials-changed"),
          harness.wake("application-active"),
          harness.wake("credentials-changed"),
        ],
        { concurrency: "unbounded" },
      );
      yield* Effect.yieldNow;

      expect(yield* Ref.get(harness.prepareCount)).toBe(1);

      yield* TestClock.adjust("15 seconds");
      const retrying = yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );

      expect(retrying).toMatchObject({
        lastFailure: {
          _tag: "ConnectionTransientError",
          reason: "timeout",
          message: "Test environment did not respond during connection setup.",
        },
      });
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);
      expect(yield* Ref.get(harness.sessionCount)).toBe(0);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("treats an involuntary session close as transient and reconnects", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.closeLatestSession();
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);

      yield* TestClock.adjust("1 second");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );

      expect(yield* Ref.get(harness.sessionCount)).toBe(2);
      expect(Option.isSome(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("keeps escalating backoff when a newly opened session flaps", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.closeLatestSession();
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );

      yield* TestClock.adjust("1 second");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );
      yield* harness.closeLatestSession();
      const secondFailure = yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 2,
      );

      expect(secondFailure.retryAt).not.toBeNull();

      yield* TestClock.adjust("1 second");
      expect(yield* Ref.get(harness.sessionCount)).toBe(2);

      yield* TestClock.adjust("1 second");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 3,
      );
      expect(yield* Ref.get(harness.sessionCount)).toBe(3);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("restarts the backoff ladder after a session stays up for thirty seconds", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.closeLatestSession();
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      yield* TestClock.adjust("1 second");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );

      // Stable this time, so the next failure waits the first delay again, not the second.
      yield* TestClock.adjust("30 seconds");
      yield* harness.closeLatestSession();
      const failure = yield* awaitState(supervisor.state, (state) => state.phase === "backoff");
      expect(failure.retryAt).toBe((yield* Clock.currentTimeMillis) + 1_000);

      yield* TestClock.adjust("1 second");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 3,
      );
      expect(yield* Ref.get(harness.sessionCount)).toBe(3);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("does not probe connected sessions for Git Manager focus wakeups", () =>
    Effect.gen(function* () {
      const probes = yield* Ref.make(0);
      const probed = yield* Deferred.make<void>();
      const harness = yield* makeHarness({
        probe: () =>
          Ref.update(probes, (count) => count + 1).pipe(
            Effect.andThen(Deferred.succeed(probed, undefined)),
          ),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.focus;
      for (let i = 0; i < 20; i += 1) yield* Effect.yieldNow;
      expect(yield* Ref.get(probes)).toBe(0);
      yield* harness.wake("application-active");
      yield* Deferred.await(probed);
      expect(yield* Ref.get(probes)).toBe(1);
    }),
  );

  it.effect("keeps a healthy session when the application becomes active", () =>
    Effect.gen(function* () {
      const probeCount = yield* Ref.make(0);
      const probeCalled = yield* Deferred.make<void>();
      const harness = yield* makeHarness({
        probe: () =>
          Ref.update(probeCount, (count) => count + 1).pipe(
            Effect.andThen(Deferred.succeed(probeCalled, undefined)),
          ),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("application-active");
      yield* Deferred.await(probeCalled);

      expect(yield* Ref.get(probeCount)).toBe(1);
      expect(yield* Ref.get(harness.sessionCount)).toBe(1);
      expect(yield* Ref.get(harness.releaseCount)).toBe(0);
      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("connected");
    }),
  );

  it.effect("reconnects when the foreground liveness probe fails", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        probe: (attempt) =>
          attempt === 1 ? Effect.fail(transient("The live session is stale.")) : Effect.void,
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("application-active");
      yield* awaitState(supervisor.state, (state) => state.phase === "backoff");
      yield* TestClock.adjust("1 second");
      yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );

      expect(yield* Ref.get(harness.sessionCount)).toBe(2);
      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("times out a stalled foreground liveness probe and reconnects", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        probe: (attempt) => (attempt === 1 ? Effect.never : Effect.void),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("application-active");
      yield* TestClock.adjust("15 seconds");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.lastFailure?.reason === "timeout",
      );
      yield* TestClock.adjust("1 second");
      yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("honors an explicit disconnect while a foreground probe is stalled", () =>
    Effect.gen(function* () {
      const probeStarted = yield* Deferred.make<void>();
      const harness = yield* makeHarness({
        probe: () => Deferred.succeed(probeStarted, undefined).pipe(Effect.andThen(Effect.never)),
      });
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("application-active");
      yield* Deferred.await(probeStarted);
      yield* supervisor.disconnect;
      yield* awaitState(supervisor.state, (state) => state.phase === "available");

      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
    }),
  );

  it.effect("does not churn a healthy session when credentials change", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("credentials-changed");
      yield* Effect.yieldNow;

      expect(yield* Ref.get(harness.sessionCount)).toBe(1);
      expect(yield* Ref.get(harness.releaseCount)).toBe(0);
      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("connected");
    }),
  );

  it.effect("releases and reconnects a relay session when credentials change", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(RELAY_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* harness.wake("credentials-changed");
      yield* awaitState(
        supervisor.state,
        (state) => state.phase === "connected" && state.generation === 2,
      );

      expect(yield* Ref.get(harness.sessionCount)).toBe(2);
      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
    }),
  );

  it.effect("interrupts relay setup when credentials change", () =>
    Effect.gen(function* () {
      const firstAttemptStarted = yield* Deferred.make<void>();
      const harness = yield* makeHarness({
        prepare: (attempt) =>
          attempt === 1
            ? Deferred.succeed(firstAttemptStarted, undefined).pipe(Effect.andThen(Effect.never))
            : Effect.succeed(PREPARED_CONNECTION),
      });
      const supervisor = yield* makeSupervisor(RELAY_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* Deferred.await(firstAttemptStarted);
      yield* harness.wake("credentials-changed");
      yield* awaitState(supervisor.state, (state) => state.phase === "connected");

      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
      expect(yield* Ref.get(harness.sessionCount)).toBe(1);
    }),
  );

  it.effect("explicit disconnect releases the session and returns to available", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* supervisor.disconnect;
      yield* awaitState(supervisor.state, (state) => state.phase === "available");

      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.session))).toBe(true);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.prepared))).toBe(true);
    }),
  );

  it.effect("does not lose an explicit disconnect among concurrent wakeup signals", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      const supervisor = yield* makeSupervisor(TARGET_ENTRY, {
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));

      yield* awaitState(supervisor.state, (state) => state.phase === "connected");
      yield* Effect.all(
        [
          supervisor.disconnect,
          harness.wake("credentials-changed"),
          harness.wake("application-active"),
          harness.wake("credentials-changed"),
        ],
        { concurrency: "unbounded" },
      );
      yield* awaitState(supervisor.state, (state) => state.phase === "available");

      expect(yield* Ref.get(harness.releaseCount)).toBe(1);
      expect(Option.isNone(yield* SubscriptionRef.get(supervisor.session))).toBe(true);
    }),
  );
  it.effect("moves every retry delay by up to fifteen percent", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({ prepare: () => Effect.fail(transient()), random: 0 });
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        targetRef: yield* Ref.make<ConnectionTarget>(TARGET_ENTRY.target),
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      const first = yield* awaitState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 1,
      );
      expect(first.retryAt! - (yield* Clock.currentTimeMillis)).toBe(850);
      yield* TestClock.adjust(849);
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);
      yield* TestClock.adjust(1);
      yield* eventuallyState(
        supervisor.state,
        (state) => state.phase === "backoff" && state.attempt === 2,
      );
      expect(yield* Ref.get(harness.prepareCount)).toBe(2);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  const collectBackoffDelays = Effect.fn("TestConnectionHarness.collectBackoffDelays")(function* (
    state: SubscriptionRef.SubscriptionRef<SupervisorConnectionState>,
    until: (delays: ReadonlyArray<number>) => boolean,
  ) {
    const delays: Array<number> = [];
    for (let attempt = 1; !until(delays); attempt += 1) {
      const backoff = yield* eventuallyState(
        state,
        (value) => value.phase === "backoff" && value.attempt === attempt,
      );
      const delay = backoff.retryAt! - (yield* Clock.currentTimeMillis);
      delays.push(delay);
      yield* TestClock.adjust(delay);
    }
    return delays;
  });

  it.effect("moves an unselected environment to the idle ladder after five minutes", () =>
    Effect.gen(function* () {
      const selection = yield* SubscriptionRef.make<EnvironmentId | null>(
        EnvironmentId.make("another-environment"),
      );
      const harness = yield* makeHarness({ prepare: () => Effect.fail(transient()), selection });
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        targetRef: yield* Ref.make<ConnectionTarget>(TARGET_ENTRY.target),
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      const delays = yield* collectBackoffDelays(
        supervisor.state,
        (collected) => collected.filter((delay) => delay >= 60_000).length === 4,
      );
      const firstIdle = delays.findIndex((delay) => delay >= 60_000);
      expect(delays.slice(firstIdle)).toEqual([60_000, 120_000, 300_000, 300_000]);
      expect(delays.slice(0, firstIdle).every((delay) => delay <= 16_000)).toBe(true);
      expect(
        delays.slice(0, firstIdle).reduce((total, delay) => total + delay, 0),
      ).toBeGreaterThanOrEqual(300_000);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("keeps the selected environment on the normal ladder", () =>
    Effect.gen(function* () {
      const selection = yield* SubscriptionRef.make<EnvironmentId | null>(TARGET.environmentId);
      const harness = yield* makeHarness({ prepare: () => Effect.fail(transient()), selection });
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        targetRef: yield* Ref.make<ConnectionTarget>(TARGET_ENTRY.target),
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      const delays = yield* collectBackoffDelays(
        supervisor.state,
        (collected) => collected.reduce((total, delay) => total + delay, 0) > 600_000,
      );
      expect(delays.every((delay) => delay <= 16_000)).toBe(true);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("ends an idle wait at once when the environment is selected", () =>
    Effect.gen(function* () {
      const selection = yield* SubscriptionRef.make<EnvironmentId | null>(
        EnvironmentId.make("another-environment"),
      );
      const harness = yield* makeHarness({ prepare: () => Effect.fail(transient()), selection });
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        targetRef: yield* Ref.make<ConnectionTarget>(TARGET_ENTRY.target),
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      const delays: Array<number> = [];
      let attempt = 1;
      for (;;) {
        const backoff = yield* eventuallyState(
          supervisor.state,
          (value) => value.phase === "backoff" && value.attempt === attempt,
        );
        const delay = backoff.retryAt! - (yield* Clock.currentTimeMillis);
        if (delay >= 60_000) break;
        delays.push(delay);
        yield* TestClock.adjust(delay);
        attempt += 1;
      }
      const before = yield* Ref.get(harness.prepareCount);
      yield* SubscriptionRef.set(selection, TARGET.environmentId);
      yield* eventuallyState(
        supervisor.state,
        (value) => value.phase === "backoff" && value.attempt === attempt + 1,
      );
      expect(yield* Ref.get(harness.prepareCount)).toBe(before + 1);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("does not restart a blocked environment when it is selected", () =>
    Effect.gen(function* () {
      const selection = yield* SubscriptionRef.make<EnvironmentId | null>(
        EnvironmentId.make("another-environment"),
      );
      const harness = yield* makeHarness({ prepare: () => Effect.fail(blocked()), selection });
      const supervisor = yield* EnvironmentSupervisor.make(TARGET_ENTRY, {
        targetRef: yield* Ref.make<ConnectionTarget>(TARGET_ENTRY.target),
        initiallyDesired: true,
      }).pipe(Effect.provide(harness.dependencies));
      yield* awaitState(supervisor.state, (state) => state.phase === "blocked");
      yield* SubscriptionRef.set(selection, TARGET.environmentId);
      for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
      expect(yield* Ref.get(harness.prepareCount)).toBe(1);
      expect((yield* SubscriptionRef.get(supervisor.state)).phase).toBe("blocked");
    }),
  );
});
