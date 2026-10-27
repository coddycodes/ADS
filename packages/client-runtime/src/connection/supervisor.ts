import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Random from "effect/Random";
import * as Ref from "effect/Ref";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as Tracer from "effect/Tracer";

import type { ConnectionCatalogEntry } from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionDriver from "./driver.ts";
import {
  type ConnectionAttemptError,
  type ConnectionTarget,
  ConnectionTransientError,
  type NetworkStatus,
  type PreparedConnection,
  type SupervisorConnectionState,
} from "./model.ts";
import { LIVENESS_TIMEOUT_SECONDS } from "../rpc/liveness.ts";
import * as RpcSession from "../rpc/session.ts";
import { safeErrorLogAttributes } from "../errors/safeLog.ts";
import * as ConnectionWakeups from "./wakeups.ts";
import { EnvironmentSelection, isEnvironmentShown } from "./selection.ts";

const RETRY_DELAYS_MS = [1_000, 2_000, 4_000, 8_000, 16_000] as const;
/** Every retry delay moves by up to ±15 % so reconnecting clients spread out. */
const RETRY_JITTER = 0.15;
/** After this much continuous failure an unselected environment retries rarely. */
const IDLE_LADDER_AFTER_MS = 5 * 60_000;
const IDLE_RETRY_DELAYS_MS = [60_000, 120_000, 300_000] as const;
/** Bounds preparation, socket opening, and authentication until configuring begins. */
export const CONNECTION_ESTABLISHMENT_TIMEOUT = "15 seconds";
/** Longest an attempt may stay in configuring while data keeps arriving. */
const CONNECTION_CONFIGURING_TIMEOUT_MS = 120_000;
/** How long configuring lasts before the slow-setup notice appears. */
const SLOW_SETUP_NOTICE_DELAY_MS = 5_000;
const CONNECTION_PROBE_TIMEOUT = "15 seconds";
const BACKOFF_RESET_AFTER_MS = 30_000;

interface SupervisorIntent {
  readonly desired: boolean;
  readonly network: NetworkStatus;
}

type SupervisorSignal =
  | { readonly _tag: "ConnectRequested" }
  | { readonly _tag: "DisconnectRequested" }
  | { readonly _tag: "RetryRequested" }
  | { readonly _tag: "NetworkChanged"; readonly network: NetworkStatus }
  | { readonly _tag: "Wakeup"; readonly reason: ConnectionWakeups.ConnectionWakeup }
  | { readonly _tag: "SelectionChanged" };

interface PendingRetryTrace {
  readonly previousAttempt: Tracer.Span;
  readonly failureCount: number;
  readonly delayMs: number;
  readonly reason: ConnectionAttemptError["reason"];
}

/**
 * The run loop's current run of failed attempts. It starts over as a whole when the user
 * disconnects and when a connection stays up long enough to count as stable.
 */
interface FailureStreak {
  /** Failed attempts so far; the next attempt is number `failureCount + 1`. */
  readonly failureCount: number;
  /** Kept visible through the next attempt. */
  readonly latestFailure: ConnectionAttemptError | null;
  /** Links the next relay attempt's trace to the one that failed. */
  readonly pendingRetry: Option.Option<PendingRetryTrace>;
  /** When the first failure of this streak happened. */
  readonly failingSince: number | null;
  /** Consecutive failures while on the idle ladder. */
  readonly idleFailures: number;
}

const NO_FAILURES: FailureStreak = {
  failureCount: 0,
  latestFailure: null,
  pendingRetry: Option.none(),
  failingSince: null,
  idleFailures: 0,
};

interface TracedAttemptFailure {
  readonly error: ConnectionAttemptError;
  readonly attemptSpan: Option.Option<Tracer.Span>;
}

type AttemptOutcome =
  | {
      readonly _tag: "Interrupted";
      readonly established: boolean;
      readonly stable: boolean;
    }
  | {
      readonly _tag: "Failure";
      readonly established: boolean;
      readonly stable: boolean;
      readonly failure: TracedAttemptFailure;
    };

type EstablishmentEvent =
  | {
      readonly _tag: "Completed";
      readonly exit: Exit.Exit<
        {
          readonly attemptSpan: Option.Option<Tracer.Span>;
          readonly lease: ConnectionDriver.EnvironmentConnectionLease;
        },
        TracedAttemptFailure
      >;
    }
  | { readonly _tag: "Interrupted" }
  | { readonly _tag: "TimedOut"; readonly detail: string };

function exitUnlessInterrupted<A, E, R>(
  effect: Effect.Effect<A, E, R>,
): Effect.Effect<Exit.Exit<A, E>, never, R> {
  return Effect.matchCauseEffect(effect, {
    onFailure: (cause) =>
      Cause.hasInterrupts(cause) ? Effect.interrupt : Effect.succeed(Exit.failCause(cause)),
    onSuccess: (value) => Effect.succeed(Exit.succeed(value)),
  });
}

export interface EnvironmentSupervisorOptions {
  readonly initiallyDesired: boolean;
  /** Registry-owned target metadata; label changes preserve this supervisor. */
  readonly targetRef: Ref.Ref<ConnectionTarget>;
}

function retryDelayMs(failureCount: number): number {
  return RETRY_DELAYS_MS[Math.min(failureCount, RETRY_DELAYS_MS.length - 1)] ?? 16_000;
}

function idleRetryDelayMs(idleFailures: number): number {
  return (
    IDLE_RETRY_DELAYS_MS[Math.min(idleFailures - 1, IDLE_RETRY_DELAYS_MS.length - 1)] ?? 300_000
  );
}

function jitteredDelayMs(
  delayMs: number,
  random: { readonly nextDoubleUnsafe: () => number },
): number {
  return Math.round(delayMs * (1 + (random.nextDoubleUnsafe() * 2 - 1) * RETRY_JITTER));
}

function annotateTarget(target: ConnectionTarget) {
  return Effect.annotateCurrentSpan({
    "environment.id": target.environmentId,
    "environment.label": target.label,
    "environment.target.kind": target._tag,
  });
}

function availableState(intent: SupervisorIntent, generation: number): SupervisorConnectionState {
  return {
    desired: false,
    network: intent.network,
    phase: "available",
    stage: null,
    attempt: 0,
    generation,
    lastFailure: null,
    retryAt: null,
  };
}

function offlineState(
  intent: SupervisorIntent,
  generation: number,
  attempt: number,
  lastFailure: ConnectionAttemptError | null,
): SupervisorConnectionState {
  return {
    desired: true,
    network: intent.network,
    phase: "offline",
    stage: null,
    attempt,
    generation,
    lastFailure,
    retryAt: null,
  };
}

function connectingState(
  intent: SupervisorIntent,
  generation: number,
  attempt: number,
  lastFailure: ConnectionAttemptError | null,
  stage: SupervisorConnectionState["stage"] = "preparing",
): SupervisorConnectionState {
  return {
    desired: true,
    network: intent.network,
    phase: "connecting",
    stage,
    attempt,
    generation,
    lastFailure,
    retryAt: null,
  };
}

/**
 * The session reports disconnects without a name; the supervisor names them
 * with the current saved catalog label when it publishes the failure.
 */
function labelDisconnectFailure(
  target: ConnectionTarget,
  error: ConnectionAttemptError,
): ConnectionAttemptError {
  if (error._tag !== "ConnectionTransientError") return error;
  const label = target.label;
  switch (error.reason) {
    case "liveness-timeout":
      return new ConnectionTransientError({
        reason: error.reason,
        detail: `No data from ${label} for ${LIVENESS_TIMEOUT_SECONDS} seconds. The connection is too slow or was lost.`,
      });
    case "connection-closed":
      return new ConnectionTransientError({
        reason: error.reason,
        detail: `${label} closed the connection.`,
      });
    case "connection-lost":
      return new ConnectionTransientError({
        reason: error.reason,
        detail: `The connection to ${label} was lost.`,
      });
    default:
      return error;
  }
}

function failureFromExit<A>(
  target: ConnectionTarget,
  exit: Exit.Exit<A, TracedAttemptFailure>,
  established: boolean,
  stable: boolean,
): AttemptOutcome {
  if (Exit.isSuccess(exit) || Cause.hasInterruptsOnly(exit.cause)) {
    return { _tag: "Interrupted", established, stable };
  }
  const typedFailure = exit.cause.reasons.find(Cause.isFailReason);
  if (typedFailure) {
    return {
      _tag: "Failure",
      established,
      stable,
      failure: {
        ...typedFailure.error,
        error: labelDisconnectFailure(target, typedFailure.error.error),
      },
    };
  }
  return {
    _tag: "Failure",
    established,
    stable,
    failure: {
      error: new ConnectionTransientError({
        reason: "transport",
        detail: `${target.label} connection failed unexpectedly.`,
      }),
      attemptSpan: Option.none(),
    },
  };
}

export class EnvironmentSupervisor extends Context.Service<
  EnvironmentSupervisor,
  {
    readonly target: ConnectionTarget;
    readonly state: SubscriptionRef.SubscriptionRef<SupervisorConnectionState>;
    readonly session: SubscriptionRef.SubscriptionRef<Option.Option<RpcSession.RpcSession>>;
    /**
     * The session read subscriptions follow (`subscribe` for the tags in
     * `PRE_VERIFICATION_SUBSCRIPTIONS`, and the shell). It is published from
     * `configuring` on, a round trip before `session`, and holds their data until the
     * connection is verified; it is the same object `session` later publishes, and it is
     * cleared with the lease. Absent on test doubles, whose followers use `session`.
     */
    readonly subscriptionSession?: SubscriptionRef.SubscriptionRef<
      Option.Option<RpcSession.RpcSession>
    >;
    readonly prepared: SubscriptionRef.SubscriptionRef<Option.Option<PreparedConnection>>;
    readonly connect: Effect.Effect<void>;
    readonly disconnect: Effect.Effect<void>;
    readonly retryNow: Effect.Effect<void>;
  }
>()("@coddy/client-runtime/connection/supervisor/EnvironmentSupervisor") {}

export const make = Effect.fn("EnvironmentSupervisor.make")(function* (
  entry: ConnectionCatalogEntry,
  options: EnvironmentSupervisorOptions,
): Effect.fn.Return<
  EnvironmentSupervisor["Service"],
  never,
  | Connectivity.Connectivity
  | ConnectionDriver.ConnectionDriver
  | Scope.Scope
  | ConnectionWakeups.ConnectionWakeups
> {
  const target = entry.target;
  const targetRef = options.targetRef;
  const currentTarget = () => Ref.getUnsafe(targetRef);
  yield* annotateTarget(target);

  const connectivity = yield* Connectivity.Connectivity;
  const driver = yield* ConnectionDriver.ConnectionDriver;
  const wakeups = yield* ConnectionWakeups.ConnectionWakeups;
  const selection = yield* EnvironmentSelection;
  const random = yield* Random.Random;
  const shown = yield* Ref.make(isEnvironmentShown(yield* selection.current, target.environmentId));
  const initialIntent: SupervisorIntent = {
    desired: options.initiallyDesired,
    network: yield* connectivity.status,
  };
  const intent = yield* Ref.make(initialIntent);
  const signals = yield* Queue.unbounded<SupervisorSignal>();
  const state = yield* SubscriptionRef.make<SupervisorConnectionState>(
    !initialIntent.desired
      ? availableState(initialIntent, 0)
      : initialIntent.network === "offline"
        ? offlineState(initialIntent, 0, 0, null)
        : connectingState(initialIntent, 0, 1, null),
  );
  const session = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(Option.none());
  const subscriptionSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
    Option.none(),
  );
  const prepared = yield* SubscriptionRef.make<Option.Option<PreparedConnection>>(Option.none());

  const clearLease = Effect.all(
    [
      SubscriptionRef.set(session, Option.none()),
      SubscriptionRef.set(subscriptionSession, Option.none()),
      SubscriptionRef.set(prepared, Option.none()),
    ],
    { discard: true },
  );

  const setState = Effect.fn("EnvironmentSupervisor.setState")(function* (
    next: SupervisorConnectionState,
  ) {
    yield* SubscriptionRef.set(state, next);
  });

  const signal = Effect.fn("EnvironmentSupervisor.signal")(function* (next: SupervisorSignal) {
    yield* Queue.offer(signals, next);
  });

  const logManagedRelayAccountChange = Effect.suspend(() =>
    Effect.logInfo("Managed relay account changed; restarting the environment connection.").pipe(
      Effect.annotateLogs({
        "environment.id": target.environmentId,
        "environment.label": currentTarget().label,
      }),
    ),
  );

  const reportProgress = Effect.fn("EnvironmentSupervisor.reportProgress")(function* (
    attempt: number,
    generation: number,
    lastFailure: ConnectionAttemptError | null,
    progress: ConnectionDriver.ConnectionDriverProgress,
  ) {
    if (
      "prepared" in progress &&
      progress.stage === "synchronizing" &&
      Option.getOrNull(yield* SubscriptionRef.get(prepared)) !== progress.prepared
    ) {
      yield* SubscriptionRef.set(prepared, Option.some(progress.prepared));
    }
    // Read subscriptions follow this from `configuring` on, alongside the config request;
    // the session holds their data until the connection is verified.
    if (
      "session" in progress &&
      progress.session !== undefined &&
      progress.stage === "configuring"
    ) {
      yield* SubscriptionRef.set(subscriptionSession, Option.some(progress.session));
    }
    yield* setState(
      connectingState(yield* Ref.get(intent), generation, attempt, lastFailure, progress.stage),
    );
  });

  const establishConnection = Effect.fnUntraced(function* (
    attempt: number,
    generation: number,
    lastFailure: ConnectionAttemptError | null,
    configuring: Deferred.Deferred<void>,
  ) {
    return yield* driver.connect({ ...entry, target: currentTarget() }, (progress) =>
      reportProgress(attempt, generation, lastFailure, progress).pipe(
        Effect.tap(() =>
          progress.stage === "configuring" ? Deferred.succeed(configuring, undefined) : Effect.void,
        ),
      ),
    );
  });

  const traceRelayEstablishment = (
    effect: Effect.Effect<
      ConnectionDriver.EnvironmentConnectionLease,
      ConnectionAttemptError,
      Scope.Scope
    >,
    attempt: number,
    generation: number,
    pendingRetry: Option.Option<PendingRetryTrace>,
  ) => {
    const traced = Effect.gen(function* () {
      const attemptSpan = yield* Effect.currentSpan.pipe(Effect.orDie);
      yield* annotateTarget(currentTarget());
      yield* Effect.annotateCurrentSpan({
        "connection.attempt": attempt,
        "connection.generation": generation,
        "connection.retry.failure_count": Option.match(pendingRetry, {
          onNone: () => 0,
          onSome: (retry) => retry.failureCount,
        }),
      });
      const lease = yield* effect.pipe(
        Effect.mapError((error): TracedAttemptFailure => ({
          error,
          attemptSpan: Option.some(attemptSpan),
        })),
      );
      return { attemptSpan: Option.some(attemptSpan), lease };
    }).pipe(Effect.withSpan("relay.connection.attempt", { root: true }));

    return Option.match(pendingRetry, {
      onNone: () => traced,
      onSome: (retry) =>
        traced.pipe(
          Effect.linkSpans(retry.previousAttempt, {
            "connection.retry.delay_ms": retry.delayMs,
            "connection.retry.reason": retry.reason,
          }),
        ),
    });
  };

  const establishTracedConnection = Effect.fnUntraced(function* (
    attempt: number,
    generation: number,
    lastFailure: ConnectionAttemptError | null,
    pendingRetry: Option.Option<PendingRetryTrace>,
    configuring: Deferred.Deferred<void>,
  ) {
    if (target._tag === "RelayConnectionTarget") {
      return yield* traceRelayEstablishment(
        establishConnection(attempt, generation, lastFailure, configuring),
        attempt,
        generation,
        pendingRetry,
      );
    }
    return yield* establishConnection(attempt, generation, lastFailure, configuring).pipe(
      Effect.map((lease) => ({
        attemptSpan: Option.none<Tracer.Span>(),
        lease,
      })),
      Effect.mapError((error): TracedAttemptFailure => ({
        error,
        attemptSpan: Option.none(),
      })),
    );
  });

  const waitForEstablishmentInterrupt = Effect.fnUntraced(function* () {
    for (;;) {
      const next = yield* Queue.take(signals);
      switch (next._tag) {
        case "DisconnectRequested":
        case "RetryRequested":
          return;
        case "NetworkChanged":
          if (next.network === "offline") {
            return;
          }
          break;
        case "ConnectRequested":
        case "SelectionChanged":
          break;
        case "Wakeup":
          if (next.reason === "credentials-changed" && target._tag === "RelayConnectionTarget") {
            yield* logManagedRelayAccountChange;
            return;
          }
          break;
      }
    }
  });

  const waitForEstablishmentDeadline = Effect.fnUntraced(function* (
    attempt: number,
    generation: number,
    configuring: Deferred.Deferred<void>,
  ): Effect.fn.Return<EstablishmentEvent> {
    const configuringStarted = yield* Deferred.await(configuring).pipe(
      Effect.as(true),
      Effect.timeoutOrElse({
        duration: CONNECTION_ESTABLISHMENT_TIMEOUT,
        orElse: () => Effect.succeed(false),
      }),
    );
    if (!configuringStarted) {
      return {
        _tag: "TimedOut",
        detail: `${currentTarget().label} did not respond during connection setup.`,
      };
    }
    yield* Effect.sleep(SLOW_SETUP_NOTICE_DELAY_MS);
    yield* SubscriptionRef.update(state, (current) =>
      current.phase === "connecting" &&
      current.stage === "configuring" &&
      current.generation === generation &&
      current.attempt === attempt
        ? {
            ...current,
            notice: `Receiving settings from ${currentTarget().label} over a slow connection…`,
          }
        : current,
    );
    yield* Effect.sleep(CONNECTION_CONFIGURING_TIMEOUT_MS - SLOW_SETUP_NOTICE_DELAY_MS);
    return {
      _tag: "TimedOut",
      detail: `${currentTarget().label} took more than ${CONNECTION_CONFIGURING_TIMEOUT_MS / 60_000} minutes to send its settings.`,
    };
  });

  const monitorConnectedLease = Effect.fnUntraced(function* (
    lease: ConnectionDriver.EnvironmentConnectionLease,
  ) {
    for (;;) {
      const next = yield* Queue.take(signals);
      switch (next._tag) {
        case "DisconnectRequested":
        case "RetryRequested":
          return;
        case "NetworkChanged":
          if (next.network === "offline") {
            return;
          }
          break;
        case "Wakeup":
          if (next.reason === "credentials-changed" && target._tag === "RelayConnectionTarget") {
            yield* logManagedRelayAccountChange;
            return;
          }
          if (next.reason === "application-active") {
            const probe = yield* lease.session.probe.pipe(
              Effect.timeoutOrElse({
                duration: CONNECTION_PROBE_TIMEOUT,
                orElse: () =>
                  Effect.fail(
                    new ConnectionTransientError({
                      reason: "timeout",
                      detail: `${currentTarget().label} did not respond to a connection health check.`,
                    }),
                  ),
              }),
              Effect.forkChild,
            );
            for (;;) {
              const probeEvent = yield* Effect.raceFirst(
                Fiber.await(probe).pipe(
                  Effect.map((exit) => ({ _tag: "ProbeCompleted" as const, exit })),
                ),
                Queue.take(signals).pipe(
                  Effect.map((signal) => ({ _tag: "Signal" as const, signal })),
                ),
              );
              if (probeEvent._tag === "ProbeCompleted") {
                yield* probeEvent.exit;
                break;
              }
              switch (probeEvent.signal._tag) {
                case "DisconnectRequested":
                case "RetryRequested":
                  yield* Fiber.interrupt(probe);
                  return;
                case "NetworkChanged":
                  if (probeEvent.signal.network === "offline") {
                    yield* Fiber.interrupt(probe);
                    return;
                  }
                  break;
                case "ConnectRequested":
                case "Wakeup":
                case "SelectionChanged":
                  break;
              }
            }
          }
          break;
        case "ConnectRequested":
        case "SelectionChanged":
          break;
      }
    }
  });

  const runAttempt = Effect.fnUntraced(function* (
    attempt: number,
    generation: number,
    lastFailure: ConnectionAttemptError | null,
    pendingRetry: Option.Option<PendingRetryTrace>,
  ) {
    yield* SubscriptionRef.set(prepared, Option.none());
    const configuring = yield* Deferred.make<void>();
    const establishment = yield* Effect.raceAllFirst([
      exitUnlessInterrupted(
        establishTracedConnection(attempt, generation, lastFailure, pendingRetry, configuring),
      ).pipe(
        Effect.map((exit): EstablishmentEvent => ({
          _tag: "Completed",
          exit,
        })),
      ),
      waitForEstablishmentInterrupt().pipe(Effect.as<EstablishmentEvent>({ _tag: "Interrupted" })),
      waitForEstablishmentDeadline(attempt, generation, configuring),
    ]);

    if (establishment._tag === "Interrupted") {
      return {
        _tag: "Interrupted",
        established: false,
        stable: false,
      } satisfies AttemptOutcome;
    }
    if (establishment._tag === "TimedOut") {
      return {
        _tag: "Failure",
        established: false,
        stable: false,
        failure: {
          error: new ConnectionTransientError({
            reason: "timeout",
            detail: establishment.detail,
          }),
          attemptSpan: Option.none(),
        },
      } satisfies AttemptOutcome;
    }
    if (Exit.isFailure(establishment.exit)) {
      const isUnexpectedDefect =
        !Cause.hasInterruptsOnly(establishment.exit.cause) &&
        !establishment.exit.cause.reasons.some(Cause.isFailReason);
      const outcome = failureFromExit(currentTarget(), establishment.exit, false, false);
      if (isUnexpectedDefect) {
        const defect = establishment.exit.cause.reasons.find(Cause.isDieReason)?.defect;
        yield* Effect.logError("Connection attempt failed with an unexpected defect.").pipe(
          Effect.annotateLogs({
            "environment.id": target.environmentId,
            "environment.label": currentTarget().label,
            "cause.reason_count": establishment.exit.cause.reasons.length,
            ...safeErrorLogAttributes(defect),
          }),
        );
      }
      return outcome;
    }

    const active = establishment.exit.value;
    const currentIntent = yield* Ref.get(intent);
    if (!currentIntent.desired || currentIntent.network === "offline") {
      return {
        _tag: "Interrupted",
        established: false,
        stable: false,
      } satisfies AttemptOutcome;
    }

    const connectedAt = yield* Clock.currentTimeMillis;
    // Usually published at configuring already: publishing the same values again would
    // make every consumer subscribe a second time.
    if (Option.getOrNull(yield* SubscriptionRef.get(prepared)) !== active.lease.prepared) {
      yield* SubscriptionRef.set(prepared, Option.some(active.lease.prepared));
    }
    if (Option.getOrNull(yield* SubscriptionRef.get(session)) !== active.lease.session) {
      yield* SubscriptionRef.set(session, Option.some(active.lease.session));
    }
    yield* setState({
      desired: true,
      network: currentIntent.network,
      phase: "connected",
      stage: null,
      attempt,
      generation,
      lastFailure: null,
      retryAt: null,
    });
    // Only now: data the session held while unverified must find the environment connected.
    yield* active.lease.open ?? Effect.void;

    const connectedExit = yield* Effect.raceFirst(
      active.lease.session.closed.pipe(
        Effect.mapError((error): TracedAttemptFailure => ({
          error,
          attemptSpan: active.attemptSpan,
        })),
      ),
      monitorConnectedLease(active.lease).pipe(
        Effect.mapError((error): TracedAttemptFailure => ({
          error,
          attemptSpan: active.attemptSpan,
        })),
      ),
    ).pipe(exitUnlessInterrupted);
    const connectedForMs = (yield* Clock.currentTimeMillis) - connectedAt;
    return failureFromExit(
      currentTarget(),
      connectedExit,
      true,
      connectedForMs >= BACKOFF_RESET_AFTER_MS,
    );
  }, Effect.ensuring(clearLease));

  const waitForRetrySignal = Effect.fnUntraced(function* (delayMs: number) {
    return yield* Effect.raceFirst(
      Effect.sleep(delayMs),
      Effect.gen(function* () {
        for (;;) {
          const next = yield* Queue.take(signals);
          switch (next._tag) {
            case "ConnectRequested":
            case "DisconnectRequested":
            case "RetryRequested":
            case "NetworkChanged":
            case "Wakeup":
            case "SelectionChanged":
              return;
          }
        }
      }),
    );
  });

  // A selection change only shortens retry waits; it never restarts an idle,
  // offline or blocked supervisor.
  const waitForSignal = Effect.gen(function* () {
    for (;;) {
      const next = yield* Queue.take(signals);
      if (next._tag !== "SelectionChanged") return next;
    }
  });

  const run = Effect.fnUntraced(function* () {
    let generation = 0;
    let streak = NO_FAILURES;

    for (;;) {
      const currentIntent = yield* Ref.get(intent);
      if (!currentIntent.desired) {
        streak = NO_FAILURES;
        yield* clearLease;
        yield* setState(availableState(currentIntent, generation));
        yield* waitForSignal;
        continue;
      }
      if (currentIntent.network === "offline") {
        yield* clearLease;
        yield* setState(
          offlineState(currentIntent, generation, streak.failureCount + 1, streak.latestFailure),
        );
        yield* waitForSignal;
        continue;
      }

      const attempt = streak.failureCount + 1;
      const nextGeneration = generation + 1;
      const outcome: AttemptOutcome = yield* Effect.scoped(
        runAttempt(attempt, nextGeneration, streak.latestFailure, streak.pendingRetry),
      );
      if (outcome.established) {
        generation = nextGeneration;
        if (outcome.stable) {
          streak = NO_FAILURES;
        }
      }
      if (outcome._tag === "Interrupted") {
        continue;
      }

      const attemptSpan: Option.Option<Tracer.Span> = outcome.failure.attemptSpan;
      const error: ConnectionAttemptError = outcome.failure.error;
      if (
        error._tag === "ConnectionBlockedError" ||
        error._tag === "ConnectionStorageChangedError"
      ) {
        streak = { ...streak, latestFailure: error };
        const blockedIntent = yield* Ref.get(intent);
        yield* setState({
          desired: blockedIntent.desired,
          network: blockedIntent.network,
          phase: "blocked",
          stage: null,
          attempt,
          generation,
          lastFailure: error,
          retryAt: null,
        });
        yield* waitForSignal;
        continue;
      }

      const failureCount = streak.failureCount + 1;
      const failedAt = yield* Clock.currentTimeMillis;
      const failingSince = streak.failingSince ?? failedAt;
      const idle = !(yield* Ref.get(shown)) && failedAt - failingSince >= IDLE_LADDER_AFTER_MS;
      const idleFailures = idle ? streak.idleFailures + 1 : 0;
      const delayMs = jitteredDelayMs(
        idle ? idleRetryDelayMs(idleFailures) : retryDelayMs(failureCount - 1),
        random,
      );
      streak = {
        failureCount,
        latestFailure: error,
        pendingRetry: Option.map(attemptSpan, (previousAttempt) => ({
          previousAttempt,
          failureCount,
          delayMs,
          reason: error.reason,
        })),
        failingSince,
        idleFailures,
      };
      const failedIntent = yield* Ref.get(intent);
      yield* setState({
        desired: failedIntent.desired,
        network: failedIntent.network,
        phase: "backoff",
        stage: null,
        attempt,
        generation,
        lastFailure: error,
        retryAt: failedAt + delayMs,
      });
      yield* waitForRetrySignal(delayMs);
    }
  });

  yield* connectivity.changes.pipe(
    Stream.runForEach((network) =>
      Ref.modify(intent, (current) =>
        current.network === network ? [false, current] : ([true, { ...current, network }] as const),
      ).pipe(
        Effect.flatMap((changed) =>
          changed ? signal({ _tag: "NetworkChanged", network }) : Effect.void,
        ),
      ),
    ),
    Effect.forkScoped,
  );
  yield* wakeups.changes.pipe(
    Stream.runForEach((reason) => signal({ _tag: "Wakeup", reason })),
    Effect.forkScoped,
  );
  yield* selection.changes.pipe(
    Stream.runForEach((selected) => {
      const nowShown = isEnvironmentShown(selected, target.environmentId);
      return Ref.getAndSet(shown, nowShown).pipe(
        Effect.flatMap((wasShown) =>
          !wasShown && nowShown ? signal({ _tag: "SelectionChanged" }) : Effect.void,
        ),
      );
    }),
    Effect.forkScoped,
  );
  yield* run().pipe(Effect.forkScoped);

  const connect = Ref.update(intent, (current) => ({
    ...current,
    desired: true,
  })).pipe(
    Effect.andThen(signal({ _tag: "ConnectRequested" })),
    Effect.withSpan("EnvironmentSupervisor.connect"),
  );

  const disconnect = Ref.update(intent, (current) => ({
    ...current,
    desired: false,
  })).pipe(
    Effect.andThen(signal({ _tag: "DisconnectRequested" })),
    Effect.withSpan("EnvironmentSupervisor.disconnect"),
  );

  const retryNow = signal({ _tag: "RetryRequested" }).pipe(
    Effect.withSpan("EnvironmentSupervisor.retryNow"),
  );

  yield* Effect.addFinalizer(() => Queue.shutdown(signals).pipe(Effect.andThen(clearLease)));

  return EnvironmentSupervisor.of({
    get target() {
      return currentTarget();
    },
    state,
    session,
    subscriptionSession,
    prepared,
    connect,
    disconnect,
    retryNow,
  });
});

export const layer = (
  entry: ConnectionCatalogEntry,
  options: Pick<EnvironmentSupervisorOptions, "initiallyDesired"> &
    Partial<Pick<EnvironmentSupervisorOptions, "targetRef">>,
): Layer.Layer<
  EnvironmentSupervisor,
  never,
  | Connectivity.Connectivity
  | ConnectionDriver.ConnectionDriver
  | ConnectionWakeups.ConnectionWakeups
> =>
  Layer.effect(
    EnvironmentSupervisor,
    Effect.gen(function* () {
      const targetRef = options.targetRef ?? (yield* Ref.make(entry.target));
      return yield* make(entry, { initiallyDesired: options.initiallyDesired, targetRef });
    }),
  );
