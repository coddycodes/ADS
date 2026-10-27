import { EnvironmentId, type EnvironmentId as EnvironmentIdType } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";

import type { SupervisorConnectionState } from "../connection/model.ts";
import { EnvironmentNotRegisteredError, EnvironmentRegistry } from "../connection/registry.ts";
import {
  type EnvironmentRpcInput,
  type EnvironmentRpcStreamFailure,
  type EnvironmentRpcStreamValue,
  type EnvironmentRpcSuccess,
  type EnvironmentStreamCommandRpcTag,
  type EnvironmentSubscriptionRpcTag,
  type EnvironmentUnaryRpcTag,
  request,
  runStream,
  subscribe,
} from "../rpc/client.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";

interface EnvironmentAtomOptions<Input, A, E, R> {
  readonly label: string;
  readonly execute: (input: Input) => Effect.Effect<A, E, R>;
  readonly timeoutMs?: number;
  readonly scheduler?: AtomCommandScheduler;
  readonly concurrency?: AtomCommandConcurrency<{
    readonly environmentId: EnvironmentIdType;
    readonly input: Input;
  }>;
}

/**
 * Background refresh for a query atom: a fixed period in milliseconds, or a policy
 * that picks the delay before the next read from each settled success and returns
 * `null` to stop refreshing until the value changes.
 */
export type EnvironmentQueryRefreshInterval<A> = number | ((value: A) => number | null);

interface EnvironmentQueryAtomOptions<Input, A, E, R> extends EnvironmentAtomOptions<
  Input,
  A,
  E,
  R
> {
  readonly staleTimeMs?: number;
  readonly idleTtlMs?: number;
  readonly refreshIntervalMs?: EnvironmentQueryRefreshInterval<A>;
  /** Stable wire-request identity when input also carries local cache metadata. */
  readonly transportCutoffKey?: (input: Input) => unknown;
}

interface EnvironmentSubscriptionAtomOptions<Input, A, E, R> {
  readonly label: string;
  readonly subscribe: (input: Input) => Stream.Stream<A, E, R>;
  readonly idleTtlMs?: number;
}

export type SettledAsyncResult<A, E> = AsyncResult.Success<A, E> | AsyncResult.Failure<A, E>;

export type AtomCommandResult<A, E> = SettledAsyncResult<A, E>;

export type AtomCommandSuccess<R> = R extends AtomCommandResult<infer A, infer _E> ? A : never;

export type AtomCommandFailure<R> = R extends AtomCommandResult<infer _A, infer E> ? E : never;

export interface AtomCommandOptions {
  readonly label?: string;
  readonly reportFailure?: boolean;
  readonly reportDefect?: boolean;
}

export interface AtomCommandReporter {
  readonly warn: (message: string, cause: Cause.Cause<unknown>) => void;
  readonly error: (message: string, cause: Cause.Cause<unknown>) => void;
}

/** Per-invocation controls, as opposed to the reporting policy in {@link AtomCommandOptions}. */
export interface AtomCommandRunOptions {
  /**
   * Aborting a running invocation settles it as interrupted at once and interrupts its effect,
   * which interrupts an in-flight RPC request. An aborted invocation that is still queued settles
   * only when its turn comes, and then does not start. Shared executions follow one signal:
   * `singleFlight` follows the caller that started the execution and ignores the signals of
   * callers that join it; `latest` follows the newest caller coalesced into a run, so that
   * caller's abort interrupts the run, or skips it while queued, for every caller in it.
   */
  readonly signal?: AbortSignal | undefined;
}

export interface AtomCommand<W, A, E> {
  readonly label: string;
  readonly run: (
    registry: AtomRegistry.AtomRegistry,
    input: W,
    options?: AtomCommandRunOptions,
  ) => Promise<AtomCommandResult<A, E>>;
}

export type AtomCommandConcurrency<W> =
  /** Every invocation runs independently. */
  | { readonly mode: "parallel" }
  | {
      /**
       * `serial` preserves every invocation in FIFO order, `singleFlight` shares an active
       * invocation, and `latest` coalesces queued invocations to the newest input.
       */
      readonly mode: "serial" | "singleFlight" | "latest";
      readonly key: (input: W) => string;
    };

interface AtomCommandSchedulerState {
  readonly serial: Map<string, Promise<unknown>>;
  readonly singleFlight: Map<string, Promise<unknown>>;
  readonly latest: Map<string, AtomCommandLatestLane>;
}

interface AtomCommandLatestBatch {
  execute: () => Promise<AtomCommandResult<unknown, unknown>>;
  readonly resolve: Array<(result: AtomCommandResult<unknown, unknown>) => void>;
}

interface AtomCommandLatestLane {
  running: boolean;
  pending: AtomCommandLatestBatch | undefined;
}

export interface AtomCommandScheduler {
  readonly schedule: <W, A, E>(
    registry: AtomRegistry.AtomRegistry,
    concurrency: AtomCommandConcurrency<W>,
    input: W,
    execute: () => Promise<AtomCommandResult<A, E>>,
  ) => Promise<AtomCommandResult<A, E>>;
}

async function settleAtomCommandResult<A, E>(
  execute: () => Promise<AtomCommandResult<A, E>>,
): Promise<AtomCommandResult<A, E>> {
  try {
    return await execute();
  } catch (defect) {
    return AsyncResult.failure(Cause.die(defect));
  }
}

export function createAtomCommandScheduler(): AtomCommandScheduler {
  const registryStates = new WeakMap<AtomRegistry.AtomRegistry, AtomCommandSchedulerState>();

  const stateFor = (registry: AtomRegistry.AtomRegistry): AtomCommandSchedulerState => {
    const existing = registryStates.get(registry);
    if (existing !== undefined) {
      return existing;
    }
    const state: AtomCommandSchedulerState = {
      serial: new Map(),
      singleFlight: new Map(),
      latest: new Map(),
    };
    registryStates.set(registry, state);
    return state;
  };

  return {
    schedule: <W, A, E>(
      registry: AtomRegistry.AtomRegistry,
      concurrency: AtomCommandConcurrency<W>,
      input: W,
      execute: () => Promise<AtomCommandResult<A, E>>,
    ): Promise<AtomCommandResult<A, E>> => {
      if (concurrency.mode === "parallel") {
        return execute();
      }

      const key = concurrency.key(input);
      const state = stateFor(registry);
      if (concurrency.mode === "singleFlight") {
        const existing = state.singleFlight.get(key) as
          | Promise<AtomCommandResult<A, E>>
          | undefined;
        if (existing !== undefined) {
          return existing;
        }
        const current = execute();
        state.singleFlight.set(key, current);
        void current.then(
          () => {
            if (state.singleFlight.get(key) === current) {
              state.singleFlight.delete(key);
            }
          },
          () => {
            if (state.singleFlight.get(key) === current) {
              state.singleFlight.delete(key);
            }
          },
        );
        return current;
      }

      if (concurrency.mode === "serial") {
        const previous = state.serial.get(key);
        const current = previous === undefined ? execute() : previous.then(execute, execute);
        state.serial.set(key, current);
        void current.then(
          () => {
            if (state.serial.get(key) === current) {
              state.serial.delete(key);
            }
          },
          () => {
            if (state.serial.get(key) === current) {
              state.serial.delete(key);
            }
          },
        );
        return current;
      }

      let lane = state.latest.get(key);
      if (lane === undefined) {
        lane = { running: false, pending: undefined };
        state.latest.set(key, lane);
      }
      const activeLane = lane;

      const result = new Promise<AtomCommandResult<A, E>>((resolve) => {
        if (activeLane.pending === undefined) {
          activeLane.pending = {
            execute: execute as () => Promise<AtomCommandResult<unknown, unknown>>,
            resolve: [resolve as (result: AtomCommandResult<unknown, unknown>) => void],
          };
          return;
        }
        activeLane.pending.execute = execute as () => Promise<AtomCommandResult<unknown, unknown>>;
        activeLane.pending.resolve.push(
          resolve as (result: AtomCommandResult<unknown, unknown>) => void,
        );
      });

      if (!activeLane.running) {
        activeLane.running = true;
        void (async () => {
          while (activeLane.pending !== undefined) {
            const batch = activeLane.pending;
            activeLane.pending = undefined;
            let batchResult: AtomCommandResult<unknown, unknown>;
            try {
              batchResult = await batch.execute();
            } catch (defect) {
              batchResult = AsyncResult.failure(Cause.die(defect));
            }
            for (const resolve of batch.resolve) {
              resolve(batchResult);
            }
          }
          activeLane.running = false;
          if (state.latest.get(key) === activeLane) {
            state.latest.delete(key);
          }
        })();
      }

      return result;
    },
  };
}

export async function runAtomCommand<W, A, E>(
  registry: AtomRegistry.AtomRegistry,
  command: AtomCommand<W, A, E>,
  input: W,
  options: AtomCommandOptions & AtomCommandRunOptions = {},
  reporter: AtomCommandReporter = console,
): Promise<AtomCommandResult<A, E>> {
  const result = await settleAtomCommandResult(() =>
    command.run(registry, input, { signal: options.signal }),
  );
  reportAtomCommandResult(result, { ...options, label: options.label ?? command.label }, reporter);
  return result;
}

export function mapAtomCommandResult<A, E, B>(
  result: AtomCommandResult<A, E>,
  map: (value: A) => B,
): AtomCommandResult<B, E> {
  return result._tag === "Success"
    ? AsyncResult.success(map(result.value))
    : AsyncResult.failure(result.cause);
}

export function isAtomCommandInterrupted(result: AtomCommandResult<unknown, unknown>): boolean {
  return result._tag === "Failure" && Cause.hasInterruptsOnly(result.cause);
}

export function squashAtomCommandFailure(result: {
  readonly cause: Cause.Cause<unknown>;
}): unknown {
  return Cause.squash(result.cause);
}

export async function settleAsyncResult<A, E>(
  execute: () => Promise<Exit.Exit<A, E>>,
): Promise<SettledAsyncResult<A, E>> {
  try {
    return AsyncResult.fromExit(await execute());
  } catch (defect) {
    return AsyncResult.failure(Cause.die(defect));
  }
}

export async function executeAtomCommand<A, E>(
  execute: () => Promise<Exit.Exit<A, E>>,
  options: AtomCommandOptions = {},
  reporter: AtomCommandReporter = console,
): Promise<AtomCommandResult<A, E>> {
  const result = await settleAsyncResult(execute);
  reportAtomCommandResult(result, options, reporter);
  return result;
}

export async function executeAtomQuery<A, E>(
  registry: AtomRegistry.AtomRegistry,
  atom: Atom.Atom<AsyncResult.AsyncResult<A, E>>,
  options: AtomCommandOptions & AtomCommandRunOptions = {},
  reporter: AtomCommandReporter = console,
): Promise<AtomCommandResult<A, E>> {
  const { signal } = options;
  const result = await settleAtomCommandResult(
    () =>
      new Promise<AtomCommandResult<A, E>>((resolve, reject) => {
        if (signal?.aborted) {
          resolve(AsyncResult.failure(Cause.interrupt()));
          return;
        }
        let settled = false;
        let unmount = () => {};
        const settle = (result: AtomCommandResult<A, E>) => {
          if (settled) return;
          settled = true;
          signal?.removeEventListener("abort", abort);
          // Unmounting disposes the atom, which interrupts its fiber when it is still running.
          unmount();
          resolve(result);
        };
        const abort = () => settle(AsyncResult.failure(Cause.interrupt()));
        const observer = Atom.make((get) => {
          get.addFinalizer(() => {
            settle(AsyncResult.failure(Cause.interrupt()));
          });
          const initial = get.once(atom);
          get.subscribe(atom, (result) => {
            if (result._tag !== "Initial" && !result.waiting) {
              settle(result);
            }
          });
          return initial;
        });

        try {
          unmount = registry.mount(observer);
          const initial = registry.get(observer);
          if (initial._tag !== "Initial" && !initial.waiting) {
            settle(initial);
          }
          if (!settled) {
            signal?.addEventListener("abort", abort, { once: true });
          }
        } catch (defect) {
          settled = true;
          unmount();
          reject(defect);
        }
      }),
  );
  reportAtomCommandResult(result, options, reporter);
  return result;
}

export function createRuntimeCommand<R, ER, W, A, E>(
  runtime: Atom.AtomRuntime<R, ER>,
  options: {
    readonly label: string;
    readonly execute: (input: W, registry: AtomRegistry.AtomRegistry) => Effect.Effect<A, E, R>;
    readonly scheduler?: AtomCommandScheduler;
    readonly concurrency?: AtomCommandConcurrency<W>;
  },
): AtomCommand<W, A, E | ER> {
  const scheduler = options.scheduler ?? createAtomCommandScheduler();
  const concurrency = options.concurrency ?? { mode: "parallel" as const };
  return {
    label: options.label,
    run: (registry, input, runOptions) =>
      settleAtomCommandResult(() =>
        scheduler.schedule(registry, concurrency, input, () => {
          const atom = runtime
            .atom(options.execute(input, registry))
            .pipe(Atom.withLabel(options.label));
          return executeAtomQuery(registry, atom, {
            reportDefect: false,
            reportFailure: false,
            signal: runOptions?.signal,
          });
        }),
      ),
  };
}

export function createRuntimeStreamCommand<R, ER, W, A, E>(
  runtime: Atom.AtomRuntime<R, ER>,
  options: {
    readonly label: string;
    readonly execute: (input: W, registry: AtomRegistry.AtomRegistry) => Stream.Stream<A, E, R>;
    readonly scheduler?: AtomCommandScheduler;
    readonly concurrency?: AtomCommandConcurrency<W>;
  },
): AtomCommand<W, A, E | ER | Cause.NoSuchElementError> {
  const scheduler = options.scheduler ?? createAtomCommandScheduler();
  const concurrency = options.concurrency ?? { mode: "parallel" as const };
  return {
    label: options.label,
    run: (registry, input, runOptions) =>
      settleAtomCommandResult(() =>
        scheduler.schedule(registry, concurrency, input, () => {
          const atom = runtime
            .atom(options.execute(input, registry))
            .pipe(Atom.withLabel(options.label));
          return executeAtomQuery(registry, atom, {
            reportDefect: false,
            reportFailure: false,
            signal: runOptions?.signal,
          });
        }),
      ),
  };
}

export function reportAtomCommandResult(
  result: AtomCommandResult<unknown, unknown>,
  options: AtomCommandOptions = {},
  reporter: AtomCommandReporter = console,
): void {
  if (AsyncResult.isSuccess(result) || Cause.hasInterruptsOnly(result.cause)) {
    return;
  }

  const label = options.label ?? "atom command";
  if (Cause.hasDies(result.cause)) {
    if (options.reportDefect ?? true) {
      reporter.error(`[atom-command] ${label} defected`, result.cause);
    }
  } else if (options.reportFailure ?? true) {
    reporter.warn(`[atom-command] ${label} failed`, result.cause);
  }
}

export async function settlePromise<A>(
  execute: () => Promise<A>,
): Promise<AtomCommandResult<A, never>> {
  try {
    return AsyncResult.success(await execute());
  } catch (defect) {
    return AsyncResult.failure(Cause.die(defect));
  }
}

export function environmentRpcKey<Input>(target: {
  readonly environmentId: EnvironmentIdType;
  readonly input: Input;
}): string {
  return JSON.stringify([target.environmentId, target.input]);
}

export function parseEnvironmentRpcKey<Input>(key: string): {
  readonly environmentId: EnvironmentIdType;
  readonly input: Input;
} {
  const decoded = JSON.parse(key) as [EnvironmentIdType, Input];
  return {
    environmentId: EnvironmentId.make(decoded[0]),
    input: decoded[1],
  };
}

export function runInEnvironment<A, E, R>(
  environmentId: EnvironmentIdType,
  effect: Effect.Effect<A, E, R>,
): Effect.Effect<
  A,
  E | EnvironmentNotRegisteredError,
  EnvironmentRegistry | Exclude<R, EnvironmentSupervisor>
> {
  return EnvironmentRegistry.pipe(
    Effect.flatMap((registry) => registry.run(environmentId, effect)),
  );
}

export function runStreamInEnvironment<A, E, R>(
  environmentId: EnvironmentIdType,
  stream: Stream.Stream<A, E, R>,
): Stream.Stream<
  A,
  E | EnvironmentNotRegisteredError,
  EnvironmentRegistry | Exclude<R, EnvironmentSupervisor>
> {
  return Stream.unwrap(
    EnvironmentRegistry.pipe(Effect.map((registry) => registry.runStream(environmentId, stream))),
  );
}

export function followStreamInEnvironment<A, E, R>(
  environmentId: EnvironmentIdType,
  stream: Stream.Stream<A, E, R>,
): Stream.Stream<A, E, EnvironmentRegistry | Exclude<R, EnvironmentSupervisor>> {
  return Stream.unwrap(
    EnvironmentRegistry.pipe(
      Effect.map((registry) => registry.followStream(environmentId, stream)),
    ),
  );
}

const isRpcClientError = Schema.is(RpcClientError.RpcClientError);

/**
 * The one rule for a query cut off by its transport: every `RpcClientError`
 * (socket closed, liveness timeout, protocol failure) counts. Views use it to
 * show "The connection dropped before the result arrived." instead of socket text.
 */
export function isQueryTransportCutoff(error: unknown): boolean {
  return isRpcClientError(error);
}
const queryRetries = new WeakMap<
  object,
  {
    readonly retry: () => void;
    readonly awaitingRetry: () => boolean;
  }
>();

/** Call only from an explicit user Retry, never from automatic invalidation. */
export function retryEnvironmentQuery(atom: object, refresh: () => void): void {
  queryRetries.get(atom)?.retry();
  refresh();
}

/**
 * Whether the rendered failure is a cut-off that only an explicit Retry sends again.
 * Pass the emission being rendered: the latch changes only together with a new
 * emission of the query, and render memoization (the React Compiler) re-reads this
 * call only when its arguments change.
 */
export function isEnvironmentQueryAwaitingRetry(
  atom: object,
  emission: AsyncResult.AsyncResult<unknown, unknown>,
): boolean {
  return emission._tag === "Failure" && (queryRetries.get(atom)?.awaitingRetry() ?? false);
}

/** A query wrapper retains the source query's explicit Retry action. */
export function forwardEnvironmentQueryRetry<A extends object>(source: object, exposed: A): A {
  const retry = queryRetries.get(source);
  if (retry !== undefined) queryRetries.set(exposed, retry);
  return exposed;
}

/**
 * One attempt of a request: the connection generation it ran on, and whether it was
 * the one automatic re-issue after a cut-off.
 */
interface QueryAttempt {
  readonly generation: number;
  readonly reissue: boolean;
}

/**
 * A recorded transport cut-off. The first one waits for a newer session, which
 * re-issues the request once; a failure of that re-issue latches.
 */
type QueryCutoff<E> =
  | {
      /** Shown until a connection of another generation arrives and re-issues the request. */
      readonly phase: "awaiting-session";
      readonly generation: number;
      readonly cause: Cause.Cause<E>;
    }
  | {
      /** The automatic budget is spent: only an explicit Retry sends the request again. */
      readonly phase: "latched";
      readonly cause: Cause.Cause<E>;
    };

interface QueryTransportState<E> {
  /** The recorded cut-off, or null while the request has none. */
  cutoff: QueryCutoff<E> | null;
  /** Set while an attempt runs; other evaluations of the request wait for it to end. */
  running: Deferred.Deferred<void> | null;
  /**
   * An attempt interrupted while its session still looked live. The next evaluation
   * decides: a newer generation means that session ended under it (the RPC client
   * interrupts a closing session's requests before the supervisor reports the drop);
   * the same generation means a refresh on the live session.
   */
  interrupted: QueryAttempt | null;
}

/** The generation of the supervisor's live session, or null while it is not connected. */
function connectedGeneration(
  state: Pick<SupervisorConnectionState, "phase" | "generation">,
): number | null {
  return state.phase === "connected" ? state.generation : null;
}

/**
 * The one rule for whether an attempt's own session is gone: the live session, if there
 * is one, has another generation. Query evaluations run only on a connected generation,
 * so they pass their own.
 */
function isAttemptSessionGone(attempt: QueryAttempt, liveGeneration: number | null): boolean {
  return liveGeneration !== attempt.generation;
}

/**
 * The cut-off recorded for an attempt interrupted after its own session ended. It is an
 * `RpcClientError`, like every other cut-off, because consumers classify a query's
 * failure by that class and not only through `isQueryTransportCutoff`:
 * `isRemoteUpdateConnectionFailure` (the server update badge) reads it as a lost
 * connection rather than as an updater failure.
 */
const sessionEndedCutoff = (): Cause.Cause<RpcClientError.RpcClientError> =>
  Cause.fail(
    new RpcClientError.RpcClientError({
      reason: new RpcClientError.RpcClientDefect({
        message: "The session ended before the result arrived.",
        cause: undefined,
      }),
    }),
  );

function isTransportCutoff(cause: Cause.Cause<unknown>): boolean {
  return cause.reasons.some(
    (reason) => Cause.isFailReason(reason) && isQueryTransportCutoff(reason.error),
  );
}

/**
 * Records how an attempt ended. Success clears the entry. A first attempt cut off
 * by its transport waits for a new session; any failure of the one re-issue
 * latches until an explicit Retry. An interrupt counts as a cut-off only when the
 * attempt's own session had ended (a view that unmounts at disconnect interrupts
 * before the RpcClientError lands); on the live session it changes nothing, so an
 * interrupted re-issue is not a failure and the next evaluation re-issues.
 */
function recordQueryOutcome<A, E>(
  state: QueryTransportState<E | RpcClientError.RpcClientError>,
  attempt: QueryAttempt,
  exit: Exit.Exit<A, E>,
  sessionEnded: boolean,
): void {
  state.interrupted = null;
  if (Exit.isSuccess(exit)) {
    state.cutoff = null;
    return;
  }
  if (Cause.hasInterruptsOnly(exit.cause) && !sessionEnded) {
    state.interrupted = attempt;
    return;
  }
  const cause: Cause.Cause<E | RpcClientError.RpcClientError> | null = Cause.hasInterruptsOnly(
    exit.cause,
  )
    ? sessionEndedCutoff()
    : attempt.reissue || isTransportCutoff(exit.cause)
      ? exit.cause
      : null;
  if (cause !== null) recordCutoff(state, attempt, cause);
}

function recordCutoff<E>(
  state: QueryTransportState<E>,
  attempt: QueryAttempt,
  cause: Cause.Cause<E>,
): void {
  state.cutoff = attempt.reissue
    ? { phase: "latched", cause }
    : { phase: "awaiting-session", generation: attempt.generation, cause };
}

/** Resolves a pending interrupted attempt against the evaluation's own generation. */
function settleInterruptedAttempt<E>(
  state: QueryTransportState<E | RpcClientError.RpcClientError>,
  generation: number,
): void {
  const interrupted = state.interrupted;
  if (interrupted === null) return;
  state.interrupted = null;
  if (isAttemptSessionGone(interrupted, generation)) {
    recordCutoff(state, interrupted, sessionEndedCutoff());
  }
}

export function createEnvironmentQueryAtomFamily<R, ER, Input, A, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: EnvironmentQueryAtomOptions<Input, A, E, EnvironmentSupervisor | R>,
): (target: {
  readonly environmentId: EnvironmentIdType;
  readonly input: Input;
}) => Atom.Atom<AsyncResult.AsyncResult<A, E | ER | Error>> {
  const rpcGenerationAtom = Atom.family((environmentId: EnvironmentIdType) =>
    runtime.atom(
      followStreamInEnvironment(
        environmentId,
        Stream.unwrap(
          EnvironmentSupervisor.pipe(
            Effect.map((supervisor) =>
              SubscriptionRef.changes(supervisor.state).pipe(
                Stream.filterMap((state) => {
                  const generation = connectedGeneration(state);
                  return generation === null ? Result.failVoid : Result.succeed(generation);
                }),
                Stream.changes,
                Stream.map<number, number | null>((generation) => generation),
              ),
            ),
          ),
        ),
      ),
      { initialValue: null },
    ),
  );
  // Cut-off bookkeeping outlives idle atom eviction; settled successful queries leave no entry.
  const cutoffsByKey = new Map<
    string,
    QueryTransportState<E | EnvironmentNotRegisteredError | RpcClientError.RpcClientError>
  >();
  const family = Atom.family((key: string) => {
    const target = parseEnvironmentRpcKey<Input>(key);
    const idleTtlMs = options.idleTtlMs ?? 5 * 60_000;
    const cutoffKey = environmentRpcKey({
      environmentId: target.environmentId,
      input: options.transportCutoffKey?.(target.input) ?? target.input,
    });
    const queryAtom = runtime
      .atom((get) => {
        const generation = Option.getOrNull(
          AsyncResult.value(get(rpcGenerationAtom(target.environmentId))),
        );
        if (generation === null) return Effect.never;
        type AttemptEffect = Effect.Effect<
          A,
          E | EnvironmentNotRegisteredError | RpcClientError.RpcClientError,
          EnvironmentRegistry | Exclude<EnvironmentSupervisor | R, EnvironmentSupervisor>
        >;
        // One attempt of the request on this evaluation's session. It is started from
        // `evaluate`'s uninterruptible step: the mark that other evaluations of the key
        // wait on is set in the same synchronous step as their check, and its finalizer
        // is installed before anything can interrupt, so the mark is always cleared.
        // When the request is interrupted, the supervisor tells whether that session
        // is gone: a newer generation, or this generation no longer connected.
        const runAttempt = (
          transport: QueryTransportState<
            E | EnvironmentNotRegisteredError | RpcClientError.RpcClientError
          >,
          reissue: boolean,
          restore: <AX, EX, RX>(effect: Effect.Effect<AX, EX, RX>) => Effect.Effect<AX, EX, RX>,
        ): AttemptEffect => {
          const done = Deferred.makeUnsafe<void>();
          transport.running = done;
          const attempt: QueryAttempt = { generation, reissue };
          let sessionEnded = false;
          const request = options.execute(target.input).pipe(
            Effect.onInterrupt(() =>
              EnvironmentSupervisor.pipe(
                Effect.flatMap((supervisor) => SubscriptionRef.get(supervisor.state)),
                Effect.map((state) => {
                  sessionEnded = isAttemptSessionGone(attempt, connectedGeneration(state));
                }),
              ),
            ),
          );
          return restore(runInEnvironment(target.environmentId, request)).pipe(
            Effect.onExit((exit) =>
              Effect.sync(() => {
                transport.running = null;
                // An explicit Retry that cleared this entry mid-flight owns the key now.
                if (cutoffsByKey.get(cutoffKey) === transport) {
                  recordQueryOutcome(transport, attempt, exit, sessionEnded);
                  if (transport.cutoff === null && transport.interrupted === null) {
                    cutoffsByKey.delete(cutoffKey);
                  }
                }
                Deferred.doneUnsafe(done, Effect.void);
              }),
            ),
          );
        };
        // Only the request and the wait for a running attempt can be interrupted; the
        // check, the decision and the mark are one uninterruptible synchronous step.
        const evaluate = (): AttemptEffect =>
          Effect.uninterruptibleMask((restore) =>
            Effect.suspend((): AttemptEffect => {
              const stored = cutoffsByKey.get(cutoffKey);
              if (stored?.running) {
                // One attempt at a time: decide once the running one has recorded its end.
                // The next decision runs inside `restore`, so its own request stays
                // interruptible.
                return restore(Deferred.await(stored.running).pipe(Effect.andThen(evaluate)));
              }
              if (stored !== undefined) {
                settleInterruptedAttempt(stored, generation);
                const cutoff = stored.cutoff;
                if (cutoff !== null) {
                  // The first cut-off stays on screen until a newer session re-issues it.
                  return cutoff.phase === "latched" || cutoff.generation === generation
                    ? Effect.failCause(cutoff.cause)
                    : runAttempt(stored, true, restore);
                }
              }
              const transport = stored ?? { cutoff: null, running: null, interrupted: null };
              cutoffsByKey.set(cutoffKey, transport);
              return runAttempt(transport, false, restore);
            }),
          );
        return evaluate();
      })
      .pipe(
        Atom.swr({
          staleTime: options.staleTimeMs ?? 30_000,
          revalidateOnMount: true,
        }),
        Atom.setIdleTTL(idleTtlMs),
      );
    const exposed = (
      options.refreshIntervalMs === undefined
        ? queryAtom
        : withQueryRefreshInterval(queryAtom, options.refreshIntervalMs)
    ).pipe(Atom.setIdleTTL(idleTtlMs), Atom.withLabel(`${options.label}:${key}`));
    queryRetries.set(exposed, {
      retry: () => {
        cutoffsByKey.delete(cutoffKey);
      },
      awaitingRetry: () => cutoffsByKey.get(cutoffKey)?.cutoff?.phase === "latched",
    });
    return exposed;
  });
  return (target) => family(environmentRpcKey(target));
}

/**
 * A fixed period re-reads after every emission (`Atom.withRefresh`). A policy arms at
 * most one timer per settled success: in-flight reads, failures, and a `null` delay arm
 * nothing, so polling stops until the value changes. The timer lives with the atom, not
 * with its readers, and re-evaluation or disposal clears it.
 */
function withQueryRefreshInterval<A, E>(
  self: Atom.Atom<AsyncResult.AsyncResult<A, E>>,
  refreshIntervalMs: EnvironmentQueryRefreshInterval<A>,
): Atom.Atom<AsyncResult.AsyncResult<A, E>> {
  if (typeof refreshIntervalMs === "number") {
    return self.pipe(Atom.withRefresh(refreshIntervalMs));
  }
  return Atom.transform(
    self,
    (get) => {
      const result = get(self);
      const delayMs =
        AsyncResult.isSuccess(result) && !result.waiting ? refreshIntervalMs(result.value) : null;
      if (delayMs !== null) {
        // @effect-diagnostics-next-line globalTimers:off - Atom reads run outside an Effect runtime; like Atom.withRefresh, the atom finalizer clears this timer.
        const handle = setTimeout(() => get.refresh(self), delayMs);
        get.addFinalizer(() => clearTimeout(handle));
      }
      return result;
    },
    { initialValueTarget: self },
  );
}

export function createEnvironmentSubscriptionAtomFamily<R, ER, Input, A, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: EnvironmentSubscriptionAtomOptions<Input, A, E, EnvironmentSupervisor | R>,
) {
  const family = Atom.family((key: string) => {
    const target = parseEnvironmentRpcKey<Input>(key);
    return runtime
      .atom(followStreamInEnvironment(target.environmentId, options.subscribe(target.input)))
      .pipe(
        Atom.setIdleTTL(options.idleTtlMs ?? 5 * 60_000),
        Atom.withLabel(`${options.label}:${key}`),
      );
  });
  return (target: { readonly environmentId: EnvironmentIdType; readonly input: Input }) =>
    family(environmentRpcKey(target));
}

export function createEnvironmentCommand<R, ER, Input, A, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: EnvironmentAtomOptions<Input, A, E, EnvironmentSupervisor | R>,
) {
  return createRuntimeCommand(runtime, {
    label: options.label,
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
    ...(options.concurrency === undefined ? {} : { concurrency: options.concurrency }),
    execute: (target) => {
      const effect = runInEnvironment(target.environmentId, options.execute(target.input));
      return options.timeoutMs === undefined
        ? effect
        : effect.pipe(Effect.timeout(options.timeoutMs));
    },
  });
}

function createEnvironmentStreamCommand<R, ER, Input, A, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: {
    readonly label: string;
    readonly execute: (input: Input) => Stream.Stream<A, E, EnvironmentSupervisor | R>;
    readonly scheduler?: AtomCommandScheduler;
    readonly concurrency?: AtomCommandConcurrency<{
      readonly environmentId: EnvironmentIdType;
      readonly input: Input;
    }>;
  },
) {
  return createRuntimeStreamCommand(runtime, {
    label: options.label,
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
    ...(options.concurrency === undefined ? {} : { concurrency: options.concurrency }),
    execute: (target) =>
      runStreamInEnvironment(target.environmentId, options.execute(target.input)).pipe(
        Stream.withSpan(options.label),
      ),
  });
}

export function createEnvironmentRpcQueryAtomFamily<R, ER, TTag extends EnvironmentUnaryRpcTag>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: {
    readonly label: string;
    readonly tag: TTag;
    readonly staleTimeMs?: number;
    readonly idleTtlMs?: number;
    readonly refreshIntervalMs?: EnvironmentQueryRefreshInterval<EnvironmentRpcSuccess<TTag>>;
  },
) {
  return createEnvironmentQueryAtomFamily(runtime, {
    label: options.label,
    ...(options.staleTimeMs === undefined ? {} : { staleTimeMs: options.staleTimeMs }),
    ...(options.idleTtlMs === undefined ? {} : { idleTtlMs: options.idleTtlMs }),
    ...(options.refreshIntervalMs === undefined
      ? {}
      : { refreshIntervalMs: options.refreshIntervalMs }),
    execute: (input: EnvironmentRpcInput<TTag>) => request(options.tag, input),
  });
}

export function createEnvironmentRpcSubscriptionAtomFamily<
  R,
  ER,
  TTag extends EnvironmentSubscriptionRpcTag,
  B = EnvironmentRpcStreamValue<TTag>,
>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: {
    readonly label: string;
    readonly tag: TTag;
    readonly idleTtlMs?: number;
    readonly transform?: (
      stream: Stream.Stream<
        EnvironmentRpcStreamValue<TTag>,
        EnvironmentRpcStreamFailure<TTag>,
        EnvironmentSupervisor | R
      >,
    ) => Stream.Stream<B, EnvironmentRpcStreamFailure<TTag>, EnvironmentSupervisor | R>;
  },
) {
  return createEnvironmentSubscriptionAtomFamily(runtime, {
    label: options.label,
    ...(options.idleTtlMs === undefined ? {} : { idleTtlMs: options.idleTtlMs }),
    subscribe: (input: EnvironmentRpcInput<TTag>) => {
      const stream = subscribe(options.tag, input);
      return options.transform === undefined
        ? (stream as Stream.Stream<B, EnvironmentRpcStreamFailure<TTag>, EnvironmentSupervisor | R>)
        : options.transform(stream);
    },
  });
}

export function createEnvironmentRpcCommand<R, ER, TTag extends EnvironmentUnaryRpcTag>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: {
    readonly label: string;
    readonly tag: TTag;
    readonly scheduler?: AtomCommandScheduler;
    readonly concurrency?: AtomCommandConcurrency<{
      readonly environmentId: EnvironmentIdType;
      readonly input: EnvironmentRpcInput<TTag>;
    }>;
  },
) {
  return createEnvironmentCommand(runtime, {
    label: options.label,
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
    ...(options.concurrency === undefined ? {} : { concurrency: options.concurrency }),
    execute: (input: EnvironmentRpcInput<TTag>) => request(options.tag, input),
  });
}

export function createEnvironmentRpcStreamCommand<
  R,
  ER,
  TTag extends EnvironmentStreamCommandRpcTag,
>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
  options: {
    readonly label: string;
    readonly tag: TTag;
    readonly scheduler?: AtomCommandScheduler;
    readonly concurrency?: AtomCommandConcurrency<{
      readonly environmentId: EnvironmentIdType;
      readonly input: EnvironmentRpcInput<TTag>;
    }>;
  },
) {
  return createEnvironmentStreamCommand(runtime, {
    label: options.label,
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
    ...(options.concurrency === undefined ? {} : { concurrency: options.concurrency }),
    execute: (input: EnvironmentRpcInput<TTag>) => runStream(options.tag, input),
  });
}
