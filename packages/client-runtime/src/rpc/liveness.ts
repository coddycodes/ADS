import * as Clock from "effect/Clock";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Random from "effect/Random";

/** Close code the client sends when it gives up on a silent connection. */
export const LIVENESS_CLOSE_CODE = 4408;
/** Close reason sent with {@link LIVENESS_CLOSE_CODE}. */
export const LIVENESS_CLOSE_REASON = "liveness timeout";

export interface LivenessTimings {
  /** Silence before the client sends an RPC Ping; also the length of each interval. */
  readonly intervalMs: number;
  /** Silent intervals after which the connection is dead. */
  readonly deadAfterIntervals: number;
  /** Relative jitter for every interval: 0.1 means ±10 %. */
  readonly jitter: number;
}

export const DEFAULT_LIVENESS_TIMINGS: LivenessTimings = {
  intervalMs: 10_000,
  deadAfterIntervals: 3,
  jitter: 0.1,
};

/**
 * How long a connection may stay silent before the client closes it, before jitter, in
 * seconds. User copy quotes it.
 */
export const LIVENESS_TIMEOUT_SECONDS =
  (DEFAULT_LIVENESS_TIMINGS.intervalMs * DEFAULT_LIVENESS_TIMINGS.deadAfterIntervals) / 1_000;

/**
 * Proof of life for one WebSocket. The session records every raw inbound
 * message here, including E2EE records before reassembly, so a large message
 * that is still arriving keeps the connection alive.
 */
export interface InboundActivity {
  /** Records one inbound message. A plain function so a DOM listener can call it. */
  readonly record: () => void;
  /** Treats now as the latest inbound message; the protocol calls it when the socket opens. */
  readonly reset: () => void;
  /** Effect-clock milliseconds of the latest `record` or `reset`. */
  readonly lastInboundAt: () => number;
  /** Increments on every `record`. */
  readonly sequence: () => number;
  /** Completes at the first `record` after the given sequence. */
  readonly awaitAfter: (sequence: number) => Effect.Effect<void>;
}

export const makeInboundActivity: Effect.Effect<InboundActivity> = Effect.gen(function* () {
  const clock = yield* Clock.Clock;
  let lastInboundAt = clock.currentTimeMillisUnsafe();
  let sequence = 0;
  let waiters: Array<Deferred.Deferred<void>> = [];
  return {
    record: () => {
      lastInboundAt = clock.currentTimeMillisUnsafe();
      sequence += 1;
      if (waiters.length === 0) return;
      const ready = waiters;
      waiters = [];
      for (const waiter of ready) Deferred.doneUnsafe(waiter, Effect.void);
    },
    reset: () => {
      lastInboundAt = clock.currentTimeMillisUnsafe();
    },
    lastInboundAt: () => lastInboundAt,
    sequence: () => sequence,
    awaitAfter: (after) =>
      Effect.suspend(() => {
        if (sequence > after) return Effect.void;
        const waiter = Deferred.makeUnsafe<void>();
        waiters.push(waiter);
        return Deferred.await(waiter).pipe(
          Effect.onInterrupt(() =>
            Effect.sync(() => {
              waiters = waiters.filter((candidate) => candidate !== waiter);
            }),
          ),
        );
      }),
  };
});

/**
 * Waits until the socket has been silent for `deadAfterIntervals` jittered
 * intervals and returns how long it was silent. After each silent interval
 * before that it sends an RPC Ping, which every live server answers. Inbound
 * messages never wake this loop: it re-reads the activity clock only when its
 * current deadline passes, so a busy socket costs one timer per interval. A
 * wake that comes late counts as one interval, never several, so a suspended
 * client cannot close a live connection the moment it resumes.
 */
export const runLivenessMonitor = <E, R>(options: {
  readonly activity: InboundActivity;
  readonly sendPing: Effect.Effect<void, E, R>;
  readonly timings?: LivenessTimings | undefined;
}): Effect.Effect<number, never, R> =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock;
    const random = yield* Random.Random;
    const timings = options.timings ?? DEFAULT_LIVENESS_TIMINGS;
    const nextInterval = (): number =>
      timings.intervalMs * (1 + (random.nextDoubleUnsafe() * 2 - 1) * timings.jitter);
    let observedSequence = options.activity.sequence();
    let silentSince = options.activity.lastInboundAt();
    let silentIntervals = 0;
    let deadline = silentSince + nextInterval();
    for (;;) {
      const now = clock.currentTimeMillisUnsafe();
      if (now < deadline) {
        yield* Effect.sleep(Math.ceil(deadline - now));
        continue;
      }
      const sequence = options.activity.sequence();
      if (sequence !== observedSequence) {
        observedSequence = sequence;
        silentSince = options.activity.lastInboundAt();
        silentIntervals = 0;
        deadline = silentSince + nextInterval();
        continue;
      }
      silentIntervals += 1;
      if (silentIntervals >= timings.deadAfterIntervals) return now - silentSince;
      yield* options.sendPing.pipe(Effect.ignore);
      // One wake counts at most one silent interval. A client that slept through several
      // (a frozen or throttled tab, a blocked main thread) gives the server a full
      // interval to answer this Ping instead of charging it the time it did not run.
      deadline = Math.max(deadline, now) + nextInterval();
    }
  });
