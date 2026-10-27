import {
  ORCHESTRATION_WS_METHODS,
  type EnvironmentId,
  type OrchestrationShellSnapshot,
  type OrchestrationShellStreamItem,
  type ServerConfig,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Ref from "effect/Ref";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import type { SupervisorConnectionState } from "../connection/model.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { safeErrorLogAttributes } from "../errors/safeLog.ts";
import {
  type ConnectionCatalogHealth,
  ConnectionCatalogHealthStore,
  EnvironmentCacheStore,
} from "../platform/persistence.ts";
import { subscribeInSession } from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";
import { applyShellStreamEvent } from "./shellReducer.ts";
import type { EnvironmentCatalogState } from "./connections.ts";
import { followStreamInEnvironment } from "./runtime.ts";

export type EnvironmentAvailabilityStatus =
  | "starting"
  | "synchronizing"
  | "live"
  | "degraded"
  | "storage-changed"
  | "recovery-required"
  | "unavailable"
  | "configuration-error";

export interface EnvironmentShellState {
  readonly snapshot: Option.Option<OrchestrationShellSnapshot>;
  readonly status: EnvironmentAvailabilityStatus;
  readonly error: Option.Option<string>;
}

const EMPTY_SHELL_STATE: EnvironmentShellState = {
  snapshot: Option.none(),
  status: "starting",
  error: Option.none(),
};

function cachedAvailabilityStatus(
  snapshot: Option.Option<OrchestrationShellSnapshot>,
): "degraded" | "unavailable" {
  return Option.isSome(snapshot) ? "degraded" : "unavailable";
}

export function resolveEnvironmentAvailabilityStatus(input: {
  readonly connection: SupervisorConnectionState;
  readonly snapshot: Option.Option<OrchestrationShellSnapshot>;
  readonly currentStatus: EnvironmentAvailabilityStatus;
}): EnvironmentAvailabilityStatus {
  const { connection, currentStatus, snapshot } = input;
  switch (connection.phase) {
    case "connecting":
      if (connection.stage === "synchronizing") {
        return "synchronizing";
      }
      return Option.isSome(snapshot) ? "degraded" : "starting";
    case "connected":
      return currentStatus === "live" ? "live" : "synchronizing";
    case "blocked":
      if (connection.lastFailure?._tag === "ConnectionStorageChangedError") {
        return "storage-changed";
      }
      if (connection.lastFailure?.reason === "recovery-required") {
        return "recovery-required";
      }
      return "configuration-error";
    case "available":
    case "offline":
    case "backoff":
      return cachedAvailabilityStatus(snapshot);
  }
}

const SHELL_SYNCHRONIZATION_ERROR_MESSAGE = "Could not synchronize environment data.";

export const makeEnvironmentShellState = Effect.fn("EnvironmentShellState.make")(function* () {
  const supervisor = yield* EnvironmentSupervisor;
  const cache = yield* EnvironmentCacheStore;
  const environmentId = supervisor.target.environmentId;
  const cachedSnapshot = yield* cache.loadShell(environmentId).pipe(
    Effect.catch((error) =>
      Effect.logWarning("Could not load cached environment shell.").pipe(
        Effect.annotateLogs({
          environmentId,
          ...safeErrorLogAttributes(error),
        }),
        Effect.as(Option.none<OrchestrationShellSnapshot>()),
      ),
    ),
  );
  const state = yield* SubscriptionRef.make<EnvironmentShellState>({
    snapshot: cachedSnapshot,
    status: Option.isSome(cachedSnapshot) ? "degraded" : "starting",
    error: Option.none(),
  });
  const stateLock = yield* Semaphore.make(1);
  const authority = yield* Ref.make<{
    readonly session: RpcSession;
    readonly generation: number;
  } | null>(null);
  const observedSession = yield* Ref.make<RpcSession | null>(
    Option.getOrNull(yield* SubscriptionRef.get(supervisor.session)),
  );
  const persistence = yield* Queue.sliding<OrchestrationShellSnapshot>(1);

  const persist = Effect.fn("EnvironmentShellState.persist")(function* (
    snapshot: OrchestrationShellSnapshot,
  ) {
    yield* cache.saveShell(environmentId, snapshot).pipe(
      Effect.catch((error) =>
        Effect.logWarning("Could not persist environment shell cache.").pipe(
          Effect.annotateLogs({
            environmentId,
            ...safeErrorLogAttributes(error),
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

  const projectConnectionStateLocked = Effect.fn(
    "EnvironmentShellState.projectConnectionStateLocked",
  )(function* (connection: SupervisorConnectionState) {
    const activeSession = Option.getOrNull(yield* SubscriptionRef.get(supervisor.session));
    const currentAuthority = yield* Ref.get(authority);
    const authorityIsCurrent =
      connection.phase === "connected" &&
      activeSession !== null &&
      currentAuthority?.session === activeSession &&
      currentAuthority.generation === connection.generation;
    if (!authorityIsCurrent) {
      yield* Ref.set(authority, null);
    }
    yield* SubscriptionRef.update(state, (current) => {
      const status = resolveEnvironmentAvailabilityStatus({
        connection,
        snapshot: current.snapshot,
        currentStatus:
          authorityIsCurrent || current.status !== "live" ? current.status : "synchronizing",
      });
      const error =
        connection.phase === "blocked" && connection.lastFailure !== null
          ? Option.some(connection.lastFailure.message)
          : Option.none<string>();
      return current.status === status &&
        Option.getOrNull(current.error) === Option.getOrNull(error)
        ? current
        : { ...current, status, error };
    });
  });
  const setStreamError = (generation: number, session: RpcSession, error: unknown) =>
    Effect.logWarning("Could not synchronize the environment shell.").pipe(
      Effect.annotateLogs({
        environmentId,
        ...safeErrorLogAttributes(error),
      }),
      Effect.andThen(
        stateLock.withPermits(1)(
          Effect.gen(function* () {
            const connection = yield* SubscriptionRef.get(supervisor.state);
            const activeSession = Option.getOrNull(yield* SubscriptionRef.get(supervisor.session));
            if (
              connection.phase !== "connected" ||
              connection.generation !== generation ||
              activeSession !== session
            ) {
              return;
            }
            yield* Ref.set(authority, null);
            yield* SubscriptionRef.update(state, (current) => ({
              ...current,
              status: cachedAvailabilityStatus(current.snapshot),
              error: Option.some(SHELL_SYNCHRONIZATION_ERROR_MESSAGE),
            }));
          }),
        ),
      ),
    );

  const applyItemLocked = Effect.fn("EnvironmentShellState.applyItemLocked")(function* (
    generation: number,
    session: RpcSession,
    item: OrchestrationShellStreamItem,
  ) {
    const connection = yield* SubscriptionRef.get(supervisor.state);
    const activeSession = Option.getOrNull(yield* SubscriptionRef.get(supervisor.session));
    if (
      connection.phase !== "connected" ||
      connection.generation !== generation ||
      activeSession !== session
    ) {
      return;
    }
    const current = yield* SubscriptionRef.get(state);
    let nextSnapshot: OrchestrationShellSnapshot | null = null;
    if (item.kind === "snapshot") {
      nextSnapshot = item.snapshot;
    } else {
      const currentAuthority = yield* Ref.get(authority);
      if (
        current.status !== "live" ||
        currentAuthority?.session !== session ||
        currentAuthority.generation !== generation ||
        Option.isNone(current.snapshot) ||
        item.sequence <= current.snapshot.value.snapshotSequence
      ) {
        return;
      }
      nextSnapshot = applyShellStreamEvent(current.snapshot.value, item);
      if (nextSnapshot === current.snapshot.value) {
        return;
      }
    }
    if (nextSnapshot === null) {
      return;
    }

    yield* Ref.set(authority, { session, generation });
    yield* SubscriptionRef.set(state, {
      snapshot: Option.some(nextSnapshot),
      status: "live",
      error: Option.none(),
    });
    yield* Queue.offer(persistence, nextSnapshot);
  });

  // The shell subscribes from `configuring` on (see `subscriptionSession`), a round trip
  // sooner; its data waits until the connection is verified and connected.
  const subscriptionSession = supervisor.subscriptionSession ?? supervisor.session;
  const connectionOrSessionChanges = SubscriptionRef.changes(supervisor.state).pipe(
    Stream.map(() => "connection" as const),
    Stream.merge(
      SubscriptionRef.changes(subscriptionSession).pipe(Stream.map(() => "session" as const)),
    ),
  );
  yield* connectionOrSessionChanges.pipe(
    Stream.mapEffect((trigger) =>
      stateLock.withPermits(1)(
        Effect.gen(function* () {
          const connection = yield* SubscriptionRef.get(supervisor.state);
          const session = Option.getOrNull(yield* SubscriptionRef.get(subscriptionSession));
          const previousSession = yield* Ref.get(observedSession);
          yield* Ref.set(observedSession, session);
          yield* projectConnectionStateLocked(connection);
          return {
            connection,
            session,
            subscribable:
              session !== null &&
              (connection.phase === "connected" || connection.phase === "connecting"),
            maySubscribe:
              trigger === "connection" || previousSession === null || previousSession === session,
          };
        }),
      ),
    ),
    Stream.changesWith(
      (previous, current) =>
        previous.subscribable === current.subscribable &&
        previous.connection.generation === current.connection.generation &&
        previous.session === current.session &&
        previous.maySubscribe === current.maySubscribe,
    ),
    Stream.switchMap(({ connection, session, subscribable, maySubscribe }) => {
      if (!subscribable || session === null || !maySubscribe) {
        return Stream.empty;
      }
      return subscribeInSession(
        session,
        environmentId,
        ORCHESTRATION_WS_METHODS.subscribeShell,
        {},
        {
          onExpectedFailure: (cause) =>
            setStreamError(connection.generation, session, Cause.squash(cause)),
        },
      ).pipe(
        Stream.map((item) => ({
          generation: connection.generation,
          session,
          item,
        })),
      );
    }),
    Stream.runForEach(({ generation, session, item }) =>
      stateLock.withPermits(1)(applyItemLocked(generation, session, item)),
    ),
    Effect.forkScoped,
  );

  return state;
});

export function shellStateChanges(environmentId: EnvironmentId) {
  return followStreamInEnvironment(
    environmentId,
    Stream.unwrap(makeEnvironmentShellState().pipe(Effect.map(SubscriptionRef.changes))),
  );
}

export interface EnvironmentShellSummary {
  readonly catalogHealth: ConnectionCatalogHealth;
  readonly catalogReady: boolean;
  readonly desiredEnvironmentCount: number;
  readonly statuses: ReadonlyArray<EnvironmentShellAvailability>;
  readonly canShowEmptyProjects: boolean;
  readonly hasSnapshot: boolean;
  readonly hasSynchronizingShell: boolean;
  readonly hasCachedShell: boolean;
  readonly hasLiveShell: boolean;
  readonly firstError: string | null;
  readonly latestSnapshotUpdatedAt: string | null;
}

export interface EnvironmentShellAvailability {
  readonly environmentId: EnvironmentId;
  readonly status: EnvironmentAvailabilityStatus;
  readonly hasSnapshot: boolean;
  readonly error: string | null;
}

const EMPTY_ENVIRONMENT_SHELL_SUMMARY: EnvironmentShellSummary = Object.freeze({
  catalogHealth: Object.freeze({ status: "ready" }),
  catalogReady: false,
  desiredEnvironmentCount: 0,
  statuses: Object.freeze([]),
  canShowEmptyProjects: false,
  hasSnapshot: false,
  hasSynchronizingShell: false,
  hasCachedShell: false,
  hasLiveShell: false,
  firstError: null,
  latestSnapshotUpdatedAt: null,
});

const EMPTY_SERVER_CONFIGS: ReadonlyMap<EnvironmentId, ServerConfig> = new Map();

function shellSummariesEqual(
  left: EnvironmentShellSummary,
  right: EnvironmentShellSummary,
): boolean {
  return (
    left.catalogHealth.status === right.catalogHealth.status &&
    (left.catalogHealth.status === "ready" ||
      (right.catalogHealth.status === "recovery-required" &&
        left.catalogHealth.message === right.catalogHealth.message)) &&
    left.catalogReady === right.catalogReady &&
    left.desiredEnvironmentCount === right.desiredEnvironmentCount &&
    left.canShowEmptyProjects === right.canShowEmptyProjects &&
    left.statuses.length === right.statuses.length &&
    left.statuses.every((status, index) => {
      const candidate = right.statuses[index];
      return (
        candidate !== undefined &&
        status.environmentId === candidate.environmentId &&
        status.status === candidate.status &&
        status.hasSnapshot === candidate.hasSnapshot &&
        status.error === candidate.error
      );
    }) &&
    left.hasSnapshot === right.hasSnapshot &&
    left.hasSynchronizingShell === right.hasSynchronizingShell &&
    left.hasCachedShell === right.hasCachedShell &&
    left.hasLiveShell === right.hasLiveShell &&
    left.firstError === right.firstError &&
    left.latestSnapshotUpdatedAt === right.latestSnapshotUpdatedAt
  );
}

function mapsEqual<K, V>(left: ReadonlyMap<K, V>, right: ReadonlyMap<K, V>): boolean {
  if (left.size !== right.size) {
    return false;
  }
  for (const [key, value] of left) {
    if (right.get(key) !== value) {
      return false;
    }
  }
  return true;
}

export function createEnvironmentShellSummaryAtom(input: {
  readonly catalogValueAtom: Atom.Atom<EnvironmentCatalogState>;
  readonly catalogHealthAtom?: Atom.Atom<ConnectionCatalogHealth>;
  readonly shellStateValueAtom: (environmentId: EnvironmentId) => Atom.Atom<EnvironmentShellState>;
}) {
  let previousSummary = EMPTY_ENVIRONMENT_SHELL_SUMMARY;
  return Atom.make((get) => {
    const catalog = get(input.catalogValueAtom);
    const catalogHealth =
      input.catalogHealthAtom === undefined
        ? ({ status: "ready" } as const)
        : get(input.catalogHealthAtom);
    const statuses: EnvironmentShellAvailability[] = [];
    let hasSnapshot = false;
    let hasSynchronizingShell = false;
    let hasCachedShell = false;
    let hasLiveShell = false;
    let firstError: string | null = null;
    let latestSnapshotUpdatedAt: string | null = null;

    for (const environmentId of catalog.entries.keys()) {
      const state = get(input.shellStateValueAtom(environmentId));
      hasSynchronizingShell ||= state.status === "synchronizing";
      hasCachedShell ||= Option.isSome(state.snapshot) && state.status !== "live";
      hasLiveShell ||= state.status === "live";
      statuses.push({
        environmentId,
        status: state.status,
        hasSnapshot: Option.isSome(state.snapshot),
        error: Option.getOrNull(state.error),
      });
      if (firstError === null) {
        firstError = Option.getOrNull(state.error);
      }
      if (Option.isNone(state.snapshot)) {
        continue;
      }
      hasSnapshot = true;
      const updatedAt = state.snapshot.value.updatedAt;
      if (latestSnapshotUpdatedAt === null || updatedAt > latestSnapshotUpdatedAt) {
        latestSnapshotUpdatedAt = updatedAt;
      }
    }

    const next: EnvironmentShellSummary = {
      catalogHealth,
      catalogReady: catalog.isReady,
      desiredEnvironmentCount: catalog.entries.size,
      statuses,
      canShowEmptyProjects:
        catalogHealth.status === "ready" &&
        catalog.isReady &&
        statuses.length > 0 &&
        statuses.every((status) => status.status === "live" && status.hasSnapshot),
      hasSnapshot,
      hasSynchronizingShell,
      hasCachedShell,
      hasLiveShell,
      firstError,
      latestSnapshotUpdatedAt,
    };
    if (shellSummariesEqual(previousSummary, next)) {
      return previousSummary;
    }
    previousSummary = next;
    return previousSummary;
  }).pipe(Atom.withLabel("environment-shell-summary"));
}

export function createConnectionCatalogHealthAtom<R, E>(
  runtime: Atom.AtomRuntime<ConnectionCatalogHealthStore | R, E>,
) {
  const healthAtom = runtime.atom(
    Stream.unwrap(
      ConnectionCatalogHealthStore.pipe(
        Effect.map((store) => SubscriptionRef.changes(store.state)),
      ),
    ),
    { initialValue: { status: "ready" } as ConnectionCatalogHealth },
  );
  return Atom.make((get) =>
    Option.getOrElse(AsyncResult.value(get(healthAtom)), () => ({ status: "ready" }) as const),
  ).pipe(Atom.withLabel("connection-catalog-health-value"));
}

export function createEnvironmentServerConfigsAtom(input: {
  readonly catalogValueAtom: Atom.Atom<EnvironmentCatalogState>;
  readonly serverConfigValueAtom: (environmentId: EnvironmentId) => Atom.Atom<ServerConfig | null>;
}) {
  let previousServerConfigs = EMPTY_SERVER_CONFIGS;
  return Atom.make((get) => {
    const next = new Map<EnvironmentId, ServerConfig>();
    for (const environmentId of get(input.catalogValueAtom).entries.keys()) {
      const config = get(input.serverConfigValueAtom(environmentId));
      if (config !== null) {
        next.set(environmentId, config);
      }
    }
    if (mapsEqual(previousServerConfigs, next)) {
      return previousServerConfigs;
    }
    previousServerConfigs = next;
    return previousServerConfigs;
  }).pipe(Atom.withLabel("environment-server-configs"));
}

export function createEnvironmentShellAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | EnvironmentCacheStore | R, E>,
) {
  const stateAtom = Atom.family((environmentId: EnvironmentId) =>
    runtime.atom(shellStateChanges(environmentId), {
      initialValue: EMPTY_SHELL_STATE,
    }),
  );

  const stateValueAtom = Atom.family((environmentId: EnvironmentId) =>
    Atom.make((get) =>
      Option.getOrElse(AsyncResult.value(get(stateAtom(environmentId))), () => EMPTY_SHELL_STATE),
    ).pipe(Atom.withLabel(`environment-shell-state-value:${environmentId}`)),
  );

  return {
    stateAtom,
    stateValueAtom,
  };
}

export * from "./models.ts";
export * from "./shellCommands.ts";
export * from "./shellReducer.ts";
export * from "./snapshots.ts";
