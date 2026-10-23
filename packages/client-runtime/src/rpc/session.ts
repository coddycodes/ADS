import {
  type E2eeAuthenticatedMessage,
  type ServerConfig,
  type ServerConfigStreamEvent,
  ORCHESTRATION_WS_METHODS,
  WS_METHODS,
} from "@bibcode/contracts";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Ref from "effect/Ref";
import type * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as RpcClient from "effect/rpc/RpcClient";
import { RpcClientDefect, RpcClientError } from "effect/rpc/RpcClientError";
import { constPing } from "effect/rpc/RpcMessage";
import * as RpcSerialization from "effect/rpc/RpcSerialization";
import * as Socket from "effect/socket/Socket";

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
    let rawSocket: (Socket.WebSocketLike & { readonly protocol?: string }) | null = null;
    const connectionWebSocketConstructor: typeof webSocketConstructor = (url, options) => {
      const socket: Socket.WebSocketLike & { readonly protocol?: string } = webSocketConstructor(
        url,
        options,
      );
      // Binary frames must stay in delivery order: E2EE ciphertext and plain records.
      // Effect's WebSocket socket also sets this; setting it here keeps the
      // invariant independent of that implementation detail.
      if ("binaryType" in socket) (socket as { binaryType: string }).binaryType = "arraybuffer";
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
          const recordE2eeFailure = (error: Socket.SocketError) => {
            const mapped = mapE2eeFailure(error);
            return mapped === null ? Effect.void : Ref.set(e2eeAttemptFailure, mapped);
          };
          // The handshake fails the reader's acquisition; a later E2EE failure
          // fails a pull. Both record the attempt's classified failure.
          return Socket.make({
            reader: encryptedSocket.reader.pipe(
              Effect.tapError(recordE2eeFailure),
              Effect.map((reader) => ({
                ...reader,
                pull: reader.pull.pipe(Effect.tapError(recordE2eeFailure)),
              })),
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

/**
 * Read-only subscriptions a session sends while its connection is still configuring
 * (the server's `read_stream` methods, except the two that open something:
 * `preview.tunnelOpen` and `previewAutomation.connect`). Their output waits until
 * the connection is verified; everything else waits to be sent.
 */
export const PRE_VERIFICATION_SUBSCRIPTIONS: ReadonlySet<string> = new Set([
  WS_METHODS.subscribeServerConfig,
  ORCHESTRATION_WS_METHODS.subscribeShell,
  ORCHESTRATION_WS_METHODS.subscribeThread,
  WS_METHODS.pullRequestsSubscribe,
  WS_METHODS.subscribeActivity,
  WS_METHODS.subscribeAuthAccess,
  WS_METHODS.subscribeDiscoveredLocalServers,
  WS_METHODS.subscribeGitManagerSignal,
  WS_METHODS.subscribePreviewEvents,
  WS_METHODS.projectsSubscribeEntries,
  WS_METHODS.subscribeServerLifecycle,
  WS_METHODS.subscribeTerminalEvents,
  WS_METHODS.subscribeTerminalMetadata,
  WS_METHODS.subscribeVcsStatus,
  WS_METHODS.subscribeVcsStatusSummary,
  WS_METHODS.subscribeWorktreeCatalog,
]);

/** Output a pre-verification subscription buffers before the connection is verified. */
const PRE_VERIFICATION_BUFFER = 16;

export interface VerifiedSessionGate {
  /** The session consumers see from `configuring` on; the same object stays live after. */
  readonly session: RpcSession;
  /** The config arrived and the identity check passed: release everything held. */
  readonly open: Effect.Effect<void>;
  /**
   * The attempt ended unverified: held calls fail and buffered output is dropped. After
   * `open` it does nothing.
   */
  readonly close: Effect.Effect<void>;
}

/**
 * Wraps a connected session so consumers can use it before its first config snapshot
 * and identity check, without a round trip of their own after them. A subscription
 * in {@link PRE_VERIFICATION_SUBSCRIPTIONS} is sent at once and buffers its output;
 * every other call, unary or mutating, is sent only once the gate opens, as before.
 * Nothing reaches a caller until then, so no data from an unverified server is
 * applied. Once open, calls go straight through.
 */
export const makeVerifiedSessionGate = (session: RpcSession) =>
  Effect.gen(function* () {
    const verified = yield* Deferred.make<void, void>();
    let opened = false;
    const unverified = () =>
      new RpcClientError({
        reason: new RpcClientDefect({
          message: "The connection ended before it was verified.",
          cause: undefined,
        }),
      });
    const awaitVerified = Deferred.await(verified).pipe(Effect.mapError(unverified));
    // Subscriptions and config reads run inside consumers that follow the published
    // session: an unverified attempt ends with that session unpublished, which
    // interrupts them. Until then they wait instead of failing, so a failed attempt
    // never surfaces as a stream error or ends a consumer that follows reconnects.
    const awaitVerifiedOrWait = Deferred.await(verified).pipe(Effect.catch(() => Effect.never));
    const client = Object.fromEntries(
      Object.entries(session.client).map(([tag, method]) => [
        tag,
        (...args: ReadonlyArray<unknown>) => {
          const result = (
            method as (
              ...values: ReadonlyArray<unknown>
            ) => Effect.Effect<unknown, RpcClientError> | Stream.Stream<unknown, RpcClientError>
          )(...args);
          if (opened) return result;
          if (!Stream.isStream(result)) return Effect.andThen(awaitVerified, result);
          if (!PRE_VERIFICATION_SUBSCRIPTIONS.has(tag)) {
            return Stream.unwrap(Effect.as(awaitVerifiedOrWait, result));
          }
          return Stream.unwrap(
            Effect.gen(function* () {
              const buffered = yield* Stream.toQueue(result, { capacity: PRE_VERIFICATION_BUFFER });
              yield* awaitVerifiedOrWait;
              return Stream.fromQueue(buffered);
            }),
          );
        },
      ]),
    ) as unknown as RpcSessionClient;
    return {
      session: {
        ...session,
        client,
        // The config is only the verified server's: consumers read it after the gate.
        initialConfig: Effect.andThen(awaitVerifiedOrWait, session.initialConfig),
        ready: Effect.andThen(awaitVerifiedOrWait, session.ready),
      },
      open: Effect.sync(() => {
        opened = true;
      }).pipe(Effect.andThen(Deferred.succeed(verified, undefined)), Effect.asVoid),
      close: Deferred.fail(verified, undefined).pipe(Effect.asVoid),
    } satisfies VerifiedSessionGate;
  });
