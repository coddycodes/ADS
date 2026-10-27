/**
 * The handler-driven socket contract Coddy Studio's socket layers are written
 * against, bridged onto Effect 4's pull-based `Socket`.
 *
 * Effect 4.0 removed `Socket.runRaw`: a socket now exposes a scoped `reader`
 * whose `pull` returns the next non-empty batch of frames. The chunked and
 * E2EE layers keep the former contract, because their ordering guarantees
 * (a compressed message never overtaken, handler effects started in arrival
 * order without waiting for each other) are defined in terms of it:
 *
 * - {@link runRaw} reads a socket into a handler. Each frame reaches the
 *   handler in arrival order; a returned effect is forked, and the run fails
 *   as soon as the socket or any forked handler effect fails. `onOpen` runs
 *   once the reader is acquired, that is once the socket is open.
 * - {@link makeRawSocket} turns such a handler-driven run back into a
 *   `Socket`. Its reader is acquired once the run calls `onOpen`, and `pull`
 *   returns the handled frames in order; after the run ends, buffered frames
 *   are still returned before the run's failure.
 */
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FiberSet from "effect/FiberSet";
import * as Queue from "effect/Queue";
import * as Scope from "effect/Scope";
import * as Socket from "effect/socket/Socket";

export type RawSocketHandler<A, E, R> = (
  data: string | Uint8Array,
) => Effect.Effect<A, E, R> | void;

export interface RawSocketRunOptions {
  readonly onOpen?: Effect.Effect<void> | undefined;
}

export type RawSocketRun = <A = void, E = never, R = never>(
  handler: RawSocketHandler<A, E, R>,
  options?: RawSocketRunOptions,
) => Effect.Effect<void, Socket.SocketError | E, R>;

/** Reads `socket` into `handler`; see the module documentation. */
export const runRaw = <A = void, E = never, R = never>(
  socket: Socket.Socket,
  handler: RawSocketHandler<A, E, R>,
  options?: RawSocketRunOptions,
): Effect.Effect<void, Socket.SocketError | E, R> =>
  Effect.scopedWith((scope) =>
    Effect.gen(function* () {
      const handlers = yield* FiberSet.make<unknown, Socket.SocketError | E>().pipe(
        Scope.provide(scope),
      );
      const run = yield* FiberSet.runtime(handlers)<R>();
      const reader = yield* Scope.provide(socket.reader, scope);
      if (options?.onOpen !== undefined) yield* options.onOpen;
      const readLoop = Effect.gen(function* () {
        while (true) {
          const frames = yield* reader.pull;
          for (const frame of frames) {
            const result = handler(frame);
            if (Effect.isEffect(result)) run(result);
          }
        }
      });
      return yield* Effect.raceFirst(readLoop, FiberSet.join(handlers));
    }),
  );

/** {@link runRaw} with binary frames decoded as UTF-8 text. */
export const runString = <A = void, E = never, R = never>(
  socket: Socket.Socket,
  handler: (data: string) => Effect.Effect<A, E, R> | void,
  options?: RawSocketRunOptions,
): Effect.Effect<void, Socket.SocketError | E, R> => {
  const decoder = new TextDecoder();
  return runRaw(
    socket,
    (data) => handler(typeof data === "string" ? data : decoder.decode(data)),
    options,
  );
};

const closedCleanly = () =>
  new Socket.SocketError({ reason: new Socket.SocketCloseError({ code: 1000 }) });

/** Builds a `Socket` from a handler-driven run; see the module documentation. */
export const makeRawSocket = (options: {
  readonly runRaw: RawSocketRun;
  readonly writer: Socket.Socket["writer"];
}): Socket.Socket =>
  Socket.make({
    reader: Effect.gen(function* () {
      const scope = yield* Effect.scope;
      const frames = yield* Queue.unbounded<string | Uint8Array, Socket.SocketError>();
      const opened = yield* Deferred.make<void, Socket.SocketError>();
      yield* options
        .runRaw(
          (data) => {
            Queue.offerUnsafe(frames, data);
          },
          { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
        )
        .pipe(
          Effect.onExit((exit) => {
            // A run only ends by failing; a clean end still reads as a close.
            const cause = Exit.isFailure(exit) ? exit.cause : Cause.fail(closedCleanly());
            return Effect.andThen(
              Deferred.failCause(opened, cause),
              Queue.failCause(frames, cause),
            );
          }),
          Effect.ignore,
          Effect.forkIn(scope),
        );
      yield* Deferred.await(opened);
      return {
        pull: Queue.takeAll(frames),
        upgrade: Socket.SocketUpgradeError.unsupported,
      };
    }),
    writer: options.writer,
  });
