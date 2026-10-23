/**
 * A local copy of Effect's `RpcClient.makeProtocolSocket`
 * (effect 4.0.2, `src/rpc/RpcClient.ts:1062-1223`; originally copied from
 * 4.0.0-beta.107) with three changes:
 *
 * - liveness counts every inbound WebSocket message instead of only `Pong`
 *   (see `liveness.ts`);
 * - the liveness close uses code 4408 and the disconnect hook receives its
 *   cause;
 * - the protocol never reconnects, because the connection supervisor owns
 *   retries.
 *
 * It reads the socket through the handler-driven `runRaw` bridge, so each
 * response's delivery starts in arrival order without waiting for the previous
 * one, as it did before Effect 4.0 made sockets pull-based.
 *
 * It uses only public Effect exports. Re-check it against upstream on every
 * Effect upgrade, and delete it once upstream exposes a configurable pinger
 * (connection-liveness design, item 1).
 */
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import { constVoid } from "effect/Function";
import * as Result from "effect/Result";
import type * as Scope from "effect/Scope";
import * as RpcClient from "effect/rpc/RpcClient";
import { RpcClientDefect, RpcClientError } from "effect/rpc/RpcClientError";
import { constPing, type FromServerEncoded } from "effect/rpc/RpcMessage";
import * as RpcSerialization from "effect/rpc/RpcSerialization";
import * as Socket from "effect/socket/Socket";

import { runRaw } from "../e2ee/rawSocket.ts";

import {
  type InboundActivity,
  LIVENESS_CLOSE_CODE,
  LIVENESS_CLOSE_REASON,
  type LivenessTimings,
  runLivenessMonitor,
} from "./liveness.ts";

/** Why a connected RPC socket ended. */
export type RpcDisconnect =
  | { readonly _tag: "LivenessTimeout"; readonly idleMs: number }
  | { readonly _tag: "Closed"; readonly code: number }
  | { readonly _tag: "Lost" };

/** Browsers report 1006 when the TCP connection ends without a close frame. */
const ABNORMAL_CLOSURE = 1006;

export function classifyDisconnect(
  cause: Cause.Cause<Socket.SocketError>,
  livenessIdleMs: number | null,
): RpcDisconnect {
  if (livenessIdleMs !== null) return { _tag: "LivenessTimeout", idleMs: livenessIdleMs };
  const error = Cause.findError(cause);
  if (
    Result.isSuccess(error) &&
    error.success.reason._tag === "SocketCloseError" &&
    error.success.reason.code !== ABNORMAL_CLOSURE
  ) {
    return { _tag: "Closed", code: error.success.reason.code };
  }
  return { _tag: "Lost" };
}

export interface LivenessProtocolOptions {
  readonly activity: InboundActivity;
  /** Runs when the socket opens; for E2EE, after in-channel authentication. */
  readonly onConnect: Effect.Effect<void>;
  /** Runs once when the socket ends, before pending calls fail. */
  readonly onDisconnect: (disconnect: RpcDisconnect) => Effect.Effect<void>;
  readonly timings?: LivenessTimings | undefined;
}

const toRpcClientError = (cause: Cause.Cause<Socket.SocketError>): RpcClientError => {
  const error = Cause.findError(cause);
  return new RpcClientError({
    reason: Result.isSuccess(error)
      ? error.success.reason
      : new RpcClientDefect({ message: "Unknown socket error", cause: Cause.squash(cause) }),
  });
};

export const makeLivenessProtocol = (
  options: LivenessProtocolOptions,
): Effect.Effect<
  RpcClient.Protocol["Service"],
  never,
  Scope.Scope | RpcSerialization.RpcSerialization | Socket.Socket
> =>
  RpcClient.Protocol.make(
    Effect.fnUntraced(function* (writeResponse, clientIds) {
      const socket = yield* Socket.Socket;
      const serialization = yield* RpcSerialization.RpcSerialization;
      const requestClientMap = new Map<string | number, number>();
      const writer = yield* socket.writer;
      const opened = yield* Deferred.make<void>();
      let parser = serialization.makeUnsafe();
      let currentError: RpcClientError | undefined;
      let livenessIdleMs: number | null = null;
      let disconnectReported = false;

      const broadcast = (response: FromServerEncoded) =>
        Effect.forEach(clientIds, (clientId) => writeResponse(clientId, response));
      const reportDisconnect = (disconnect: RpcDisconnect) =>
        Effect.suspend(() => {
          if (disconnectReported) return Effect.void;
          disconnectReported = true;
          return options.onDisconnect(disconnect);
        });

      const onOpen = Effect.suspend(() => {
        currentError = undefined;
        options.activity.reset();
        Deferred.doneUnsafe(opened, Effect.void);
        return options.onConnect;
      });

      const handleMessage = (message: string | Uint8Array) => {
        try {
          const responses = parser.decode(message) as Array<FromServerEncoded>;
          if (responses.length === 0) return;
          let index = 0;
          return Effect.whileLoop({
            while: () => index < responses.length,
            body: () => {
              const response = responses[index++]!;
              // A Pong only proves liveness, which the raw message already recorded.
              if (response._tag === "Pong") return Effect.void;
              if (Object.hasOwn(response, "requestId")) {
                const requestId = (
                  response as FromServerEncoded & { readonly requestId: string | number }
                ).requestId;
                const clientId = requestClientMap.get(requestId);
                if (clientId !== undefined) {
                  if (response._tag === "Exit") requestClientMap.delete(requestId);
                  return writeResponse(clientId, response);
                }
              }
              return broadcast(response);
            },
            step: constVoid,
          });
        } catch (defect) {
          return broadcast({
            _tag: "ClientProtocolError",
            error: new RpcClientError({
              reason: new RpcClientDefect({ message: "Error decoding message", cause: defect }),
            }),
          });
        }
      };

      const sendPing = Effect.suspend(() => {
        const encoded = parser.encode(constPing);
        return encoded === undefined ? Effect.void : writer.write(encoded);
      });

      const livenessTimeout = Deferred.await(opened).pipe(
        Effect.andThen(
          runLivenessMonitor({ activity: options.activity, sendPing, timings: options.timings }),
        ),
        Effect.flatMap((idleMs) => {
          livenessIdleMs = idleMs;
          return Effect.logWarning("liveness-timeout").pipe(
            Effect.annotateLogs({ "rpc.liveness.idle_ms": idleMs }),
            // The close code must leave before the scope's release closes with 1000.
            Effect.andThen(
              writer
                .write(new Socket.CloseEvent(LIVENESS_CLOSE_CODE, LIVENESS_CLOSE_REASON))
                .pipe(Effect.ignore),
            ),
            Effect.andThen(
              Effect.fail(
                new Socket.SocketError({
                  reason: new Socket.SocketCloseError({
                    code: LIVENESS_CLOSE_CODE,
                    closeReason: LIVENESS_CLOSE_REASON,
                  }),
                }),
              ),
            ),
          );
        }),
      );

      yield* Effect.suspend(() => {
        parser = serialization.makeUnsafe();
        return runRaw(socket, handleMessage, { onOpen }).pipe(Effect.raceFirst(livenessTimeout));
      }).pipe(
        Effect.flatMap(() =>
          Effect.fail(
            new Socket.SocketError({ reason: new Socket.SocketCloseError({ code: 1000 }) }),
          ),
        ),
        // Teardown order: fail new sends at once instead of waiting on the
        // closed socket; publish the classified disconnect, so `ready` and
        // `probe` report it; only then fail pending calls with the generic
        // `RpcClientError`, which callers such as the clone re-attach treat as
        // a transport loss.
        Effect.tapCause((cause) => {
          const error = toRpcClientError(cause);
          currentError = error;
          return reportDisconnect(classifyDisconnect(cause, livenessIdleMs)).pipe(
            Effect.andThen(broadcast({ _tag: "ClientProtocolError", error })),
          );
        }),
        Effect.onExit((exit) =>
          reportDisconnect(
            Exit.isFailure(exit)
              ? classifyDisconnect(exit.cause, livenessIdleMs)
              : { _tag: "Lost" },
          ),
        ),
        Effect.ignore,
        Effect.annotateLogs({ module: "RpcClient", method: "makeLivenessProtocol" }),
        Effect.forkScoped,
      );

      return {
        send(clientId, request) {
          if (currentError) return Effect.fail(currentError);
          if (request._tag === "Request") requestClientMap.set(request.id, clientId);
          const encoded = parser.encode(request);
          if (encoded === undefined) return Effect.void;
          return Effect.orDie(writer.write(encoded));
        },
        supportsAck: true,
        supportsTransferables: false,
        codecFor: serialization.codecFor,
      };
    }),
  );
