import {
  type E2eeAuthenticatedMessage,
  type ServerConfig,
  type ServerConfigStreamEvent,
  WS_METHODS,
} from "@bibcode/contracts";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Ref from "effect/Ref";
import type * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as RpcClient from "effect/unstable/rpc/RpcClient";
import { constPing } from "effect/unstable/rpc/RpcMessage";
import * as RpcSerialization from "effect/unstable/rpc/RpcSerialization";
import * as Socket from "effect/unstable/socket/Socket";

import { makeWsRpcProtocolClient, type WsRpcProtocolClient } from "./protocol.ts";
import { e2eeFailureOf, makeE2eeSocket } from "../e2ee/index.ts";
import type {
  ConnectionAttemptError,
  ConnectionTransientReason,
  PreparedConnection,
} from "../connection/model.ts";
import {
  ConnectionBlockedError,
  ConnectionTransientError as ConnectionTransientErrorClass,
} from "../connection/model.ts";

import { makeChunkedSocket, plainSocketProtocols } from "./chunkedSocket.ts";
import { withInputAdmission } from "./inputAdmission.ts";
import { makeInboundActivity } from "./liveness.ts";
import { makeLivenessProtocol, type RpcDisconnect } from "./livenessProtocol.ts";
import { makeSharedServerConfig } from "./sharedServerConfig.ts";

const SOCKET_OPEN_TIMEOUT = "15 seconds";

type ServerConfigSubscription = WsRpcProtocolClient[typeof WS_METHODS.subscribeServerConfig];

/** Failures of the config subscription that readiness waits on. */
type InitialConfigError = ServerConfigSubscription extends (
  input: any,
  options?: any,
) => Stream.Stream<any, infer E, any>
  ? E
  : never;

/**
 * The session's RPC client. Its `subscribeServerConfig` replays the connection's one config
 * stream instead of opening another, so it has only the stream form: no `asQueue`, and no
 * per-call options.
 */
export type RpcSessionClient = Omit<
  WsRpcProtocolClient,
  typeof WS_METHODS.subscribeServerConfig
> & {
  readonly [WS_METHODS.subscribeServerConfig]: (
    input: Parameters<ServerConfigSubscription>[0],
  ) => Stream.Stream<ServerConfigStreamEvent, InitialConfigError>;
};

export interface RpcSession {
  readonly client: RpcSessionClient;
  readonly initialConfig: Effect.Effect<ServerConfig, ConnectionAttemptError>;
  readonly ready: Effect.Effect<void, ConnectionAttemptError>;
  readonly probe: Effect.Effect<void, ConnectionAttemptError>;
  readonly closed: Effect.Effect<never, ConnectionAttemptError>;
  readonly e2eeAuthenticated: Effect.Effect<E2eeAuthenticatedMessage | null>;
}

/**
 * Factory return type: `connected` completes when the socket connects (for E2EE,
 * after authentication), or fails with the disconnect if the socket ends first.
 * Live-session consumers use `RpcSession`.
 */
export interface EstablishingRpcSession extends RpcSession {
  readonly connected: Effect.Effect<void, ConnectionAttemptError>;
}

export class RpcSessionFactory extends Context.Service<
  RpcSessionFactory,
  {
    readonly connect: (
      connection: PreparedConnection,
    ) => Effect.Effect<EstablishingRpcSession, ConnectionAttemptError, Scope.Scope>;
  }
>()("@bibcode/client-runtime/rpc/session/RpcSessionFactory") {}

function mapInitialConfigError(error: InitialConfigError): ConnectionAttemptError {
  switch (error._tag) {
    case "EnvironmentAuthorizationError":
      return new ConnectionBlockedError({
        reason: "permission",
        detail: error.message,
      });
    case "UpdateMaintenanceActiveError":
    case "KeybindingsConfigParseError":
    case "ServerSettingsError":
    case "RpcResponseTooLargeError":
      return new ConnectionTransientErrorClass({
        reason: "remote-unavailable",
        detail: error.message,
      });
    case "RpcClientError":
      return new ConnectionTransientErrorClass({
        reason: "transport",
        detail: error.message,
      });
  }
}

function mapE2eeFailure(error: unknown): ConnectionAttemptError | null {
  const failure = e2eeFailureOf(error);
  if (failure === null) return null;
  switch (failure.reason) {
    case "host-identity-mismatch":
      return new ConnectionBlockedError({
        reason: "host-identity",
        detail: "The remote host identity does not match the saved pairing.",
      });
    case "unauthorized":
      return new ConnectionBlockedError({
        reason: "authentication",
        detail: "The remote environment rejected the saved credential.",
      });
    case "timeout":
      return new ConnectionTransientErrorClass({
        reason: "timeout",
        detail: "The encrypted channel handshake timed out.",
      });
    case "protocol":
      return new ConnectionTransientErrorClass({
        reason: "transport",
        detail: "The encrypted channel protocol failed.",
      });
  }
}

/** How each end of an established connection is reported; the supervisor adds the name. */
const DISCONNECT_REASONS = {
  LivenessTimeout: "liveness-timeout",
  Closed: "connection-closed",
  Lost: "connection-lost",
} as const satisfies Record<RpcDisconnect["_tag"], ConnectionTransientReason>;

function disconnectError(
  wasConnected: boolean,
  disconnect: RpcDisconnect,
): ConnectionTransientErrorClass {
  if (!wasConnected) {
    return new ConnectionTransientErrorClass({
      reason: "transport",
      detail: "Could not establish a WebSocket connection.",
    });
  }
  return new ConnectionTransientErrorClass({
    reason: DISCONNECT_REASONS[disconnect._tag],
    detail: "The connection disconnected.",
  });
}

export const make = Effect.gen(function* () {
  const webSocketConstructor = yield* Socket.WebSocketConstructor;

  const connect = Effect.fnUntraced(function* (connection: PreparedConnection) {
    yield* Effect.annotateCurrentSpan({
      "connection.environment.id": connection.environmentId,
    });

    const connected = yield* Deferred.make<void>();
    const disconnected = yield* Deferred.make<never, ConnectionAttemptError>();
    const e2eeAuthenticated = yield* Deferred.make<E2eeAuthenticatedMessage | null>();
    const e2eeAttemptFailure = yield* Ref.make<ConnectionAttemptError | null>(null);
    const activity = yield* makeInboundActivity;
    const onDisconnect = (disconnect: RpcDisconnect) =>
      Effect.all({
        wasConnected: Deferred.isDone(connected),
        e2eeFailure: Ref.get(e2eeAttemptFailure),
      }).pipe(
        Effect.flatMap(({ wasConnected, e2eeFailure }) =>
          Deferred.fail(disconnected, e2eeFailure ?? disconnectError(wasConnected, disconnect)),
        ),
        Effect.asVoid,
      );
    let rawSocket: globalThis.WebSocket | null = null;
    const connectionWebSocketConstructor: typeof webSocketConstructor = (url, protocols) => {
      const socket = webSocketConstructor(url, protocols);
      // Binary frames must stay in delivery order: E2EE ciphertext and plain records.
      socket.binaryType = "arraybuffer";
      // Every raw frame is proof of life, including E2EE records before reassembly.
      socket.addEventListener("message", activity.record);
      rawSocket = socket;
      return socket;
    };
    const socketLayer = Layer.effect(
      Socket.Socket,
      Socket.makeWebSocket(connection.socketUrl, {
        openTimeout: SOCKET_OPEN_TIMEOUT,
        ...(connection.e2ee === null
          ? {
              protocols: plainSocketProtocols({
                socketUrl: connection.socketUrl,
                tunnelled: connection.target._tag === "SshConnectionTarget",
              }),
            }
          : {}),
      }).pipe(
        Effect.map((plainSocket) => {
          if (connection.e2ee === null) {
            return makeChunkedSocket(plainSocket, () => rawSocket?.protocol);
          }
          const encryptedSocket = makeE2eeSocket(plainSocket, {
            hostKey: connection.e2ee.hostKey,
            auth: connection.e2ee.auth,
            onAuthenticated: (message) => {
              Deferred.doneUnsafe(e2eeAuthenticated, Effect.succeed(message));
            },
          });
          return Socket.make({
            runRaw: (handler, options) =>
              encryptedSocket.runRaw(handler, options).pipe(
                Effect.tapError((error) => {
                  const mapped = mapE2eeFailure(error);
                  return mapped === null ? Effect.void : Ref.set(e2eeAttemptFailure, mapped);
                }),
              ),
            writer: encryptedSocket.writer,
          });
        }),
      ),
    ).pipe(
      Layer.provide(Layer.succeed(Socket.WebSocketConstructor, connectionWebSocketConstructor)),
    );
    // Layer.build keeps the socket in this session's scope; Effect.provide(layer) would close it.
    const transportContext = yield* Layer.build(
      Layer.mergeAll(socketLayer, RpcSerialization.layerJson),
    );
    const protocol = yield* makeLivenessProtocol({
      activity,
      onConnect: Deferred.succeed(connected, undefined).pipe(Effect.asVoid),
      onDisconnect,
    }).pipe(Effect.provide(transportContext), Effect.withSpan("environment.websocket.connect"));
    const admitted = withInputAdmission(
      yield* makeWsRpcProtocolClient.pipe(Effect.provideService(RpcClient.Protocol, protocol)),
    );
    // The connection's one config stream: readiness waits for its first
    // snapshot, and later subscribers replay it instead of opening another.
    // It starts once the socket is connected (for E2EE, authenticated), so a
    // session torn down before that never waits to send its interrupt.
    const sharedConfig = yield* makeSharedServerConfig(
      Stream.unwrap(
        Deferred.await(connected).pipe(Effect.as(admitted[WS_METHODS.subscribeServerConfig]({}))),
      ),
    );
    // Later subscribers replay the shared stream (see `RpcSessionClient`).
    const client: RpcSessionClient = {
      ...admitted,
      [WS_METHODS.subscribeServerConfig]: () => sharedConfig.events,
    };
    // The protocol publishes the classified disconnect before it fails sends
    // and pending calls, so a transport failure seen after the socket ended
    // reports that disconnect. Typed server answers keep their own mapping.
    const failWithDisconnectOr = (fallback: ConnectionAttemptError) =>
      Deferred.isDone(disconnected).pipe(
        Effect.flatMap((done) => (done ? Deferred.await(disconnected) : Effect.fail(fallback))),
      );
    const mapSessionRequestError = (error: InitialConfigError) =>
      error._tag === "RpcClientError"
        ? failWithDisconnectOr(mapInitialConfigError(error))
        : Effect.fail(mapInitialConfigError(error));
    const initialConfig = yield* Effect.cached(
      sharedConfig.initialConfig.pipe(
        Effect.catch(mapSessionRequestError),
        Effect.withSpan("environment.initialSync"),
      ),
    );
    // Any inbound message after the Ping proves the connection is alive.
    const probe = Effect.suspend(() => {
      const before = activity.sequence();
      return protocol.send(0, constPing).pipe(Effect.andThen(activity.awaitAfter(before)));
    }).pipe(
      Effect.catch((error) =>
        failWithDisconnectOr(
          new ConnectionTransientErrorClass({
            reason: "transport",
            detail: error.message,
          }),
        ),
      ),
      Effect.raceFirst(Deferred.await(disconnected)),
      Effect.withSpan("clientRuntime.connection.rpcSession.probe"),
    );

    return {
      client,
      initialConfig,
      connected: Deferred.await(connected).pipe(Effect.raceFirst(Deferred.await(disconnected))),
      ready: Deferred.await(connected).pipe(
        Effect.andThen(initialConfig),
        Effect.asVoid,
        Effect.raceFirst(Deferred.await(disconnected)),
      ),
      probe,
      closed: Deferred.await(disconnected),
      e2eeAuthenticated:
        connection.e2ee === null ? Effect.succeed(null) : Deferred.await(e2eeAuthenticated),
    } satisfies EstablishingRpcSession;
  });

  return RpcSessionFactory.of({ connect });
});

export const layer = Layer.effect(RpcSessionFactory, make);
