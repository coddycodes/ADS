import * as NodeZlib from "node:zlib";

import {
  DEFAULT_SERVER_SETTINGS,
  EnvironmentId,
  RpcResponseTooLargeError,
  ServerConfig,
  ServerSettings,
  ThreadId,
  type ServerConfig as ServerConfigType,
  WS_METHODS,
} from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Random from "effect/Random";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";
import * as Socket from "effect/socket/Socket";

import { splitIntoRecords } from "../e2ee/frame.ts";
import { createNkResponder, derivePublicKey } from "../e2ee/noise.ts";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
  PrimaryConnectionTarget,
  type PreparedConnection,
} from "../connection/model.ts";
import * as RpcSession from "./session.ts";
import { createOrderedTerminalInputBinding } from "../state/orderedTerminalInput.ts";

type SocketEventType = "open" | "message" | "close" | "error";
type SocketEvent = {
  readonly code?: number;
  readonly data?: unknown;
  readonly reason?: string;
  readonly type: SocketEventType;
};
type SocketListener = (event: SocketEvent) => void;

class TestWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = TestWebSocket.CONNECTING;
  binaryType: BinaryType = "blob";
  protocol = "";
  readonly sent: Array<string | Uint8Array> = [];
  readonly url: string;
  readonly protocols: string | Array<string> | undefined;
  closeCode: number | null = null;
  closeReason: string | null = null;
  private readonly listeners = new Map<SocketEventType, Set<SocketListener>>();

  constructor(url: string, protocols?: string | Array<string>) {
    this.url = url;
    this.protocols = protocols;
  }

  addEventListener(type: SocketEventType, listener: SocketListener) {
    const listeners = this.listeners.get(type) ?? new Set<SocketListener>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: SocketEventType, listener: SocketListener) {
    this.listeners.get(type)?.delete(listener);
  }

  send(data: string | Uint8Array) {
    this.sent.push(data);
  }

  close(code = 1000, reason = "") {
    if (this.readyState === TestWebSocket.CLOSED) {
      return;
    }
    this.closeCode = code;
    this.closeReason = reason;
    this.readyState = TestWebSocket.CLOSED;
    this.emit("close", { code, reason, type: "close" });
  }

  open() {
    this.readyState = TestWebSocket.OPEN;
    this.emit("open", { type: "open" });
  }

  serverMessage(data: string | Uint8Array) {
    this.emit("message", { data, type: "message" });
  }

  private emit(type: SocketEventType, event: SocketEvent) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});

const PREPARED: PreparedConnection = {
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
  socketUrl: "wss://environment.example.test/ws?wsTicket=test",
  httpAuthorization: null,
  e2ee: null,
  target: TARGET,
};

const SERVER_CONFIG: ServerConfigType = {
  environment: {
    environmentId: TARGET.environmentId,
    label: TARGET.label,
    platform: {
      os: "darwin",
      arch: "arm64",
    },
    serverVersion: "0.0.0-test",
    storageInstanceId: null,
    bootId: null,
    remoteUpdateSupport: null,
    remoteProtocolVersion: 1,
    minCompatibleRemoteProtocol: 1,
    capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
  },
  auth: {
    policy: "loopback-browser",
    bootstrapMethods: ["one-time-token"],
    sessionMethods: ["browser-session-cookie", "bearer-access-token"],
    sessionCookieName: "bibcode_session",
  },
  cwd: "/tmp/workspace",
  keybindingsConfigPath: "/tmp/workspace/keybindings.json",
  keybindings: [],
  issues: [],
  providers: [],
  availableEditors: [],
  observability: {
    logsDirectoryPath: "/tmp/logs",
    localTracingEnabled: false,
    otlpTracesEnabled: false,
    otlpMetricsEnabled: false,
  },
  settings: DEFAULT_SERVER_SETTINGS,
};

const RpcRequest = Schema.TaggedStruct("Request", {
  id: Schema.String,
  payload: Schema.Unknown,
  tag: Schema.String,
});
const decodeJson = Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Unknown));
const decodeRpcRequest = Schema.decodeUnknownSync(RpcRequest);
const encodeJson = Schema.encodeUnknownSync(Schema.fromJsonString(Schema.Unknown));
const encodeServerConfig = Schema.encodeSync(ServerConfig);
const encodeServerSettings = Schema.encodeSync(ServerSettings);
const PING_FRAME = JSON.stringify({ _tag: "Ping" });
const FIXED_RANDOM = { nextDoubleUnsafe: () => 0.5, nextIntUnsafe: () => 0 };
const withFixedRandom = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.provideService(Random.Random, FIXED_RANDOM));
const pingsSent = (socket: TestWebSocket) =>
  socket.sent.filter((frame) => frame === PING_FRAME).length;

const makeFactory = Effect.fn("TestRpcSessionFactory.make")(function* () {
  const sockets: TestWebSocket[] = [];
  const constructorLayer = Layer.succeed(Socket.WebSocketConstructor, (url, options) => {
    if (options !== undefined && typeof options !== "string" && !Array.isArray(options)) {
      throw new TypeError("The test WebSocket accepts only subprotocols.");
    }
    const socket = new TestWebSocket(url, options);
    sockets.push(socket);
    return socket;
  });
  const layer = RpcSession.layer.pipe(Layer.provide(constructorLayer));
  const factory = yield* RpcSession.RpcSessionFactory.pipe(Effect.provide(layer));
  return { factory, sockets };
});

const awaitSocket = Effect.fn("TestRpcSessionFactory.awaitSocket")(function* (
  sockets: ReadonlyArray<TestWebSocket>,
) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const socket = sockets[0];
    if (socket) {
      return socket;
    }
    yield* Effect.yieldNow;
  }
  return yield* Effect.die(new Error("Expected the RPC protocol to create a websocket."));
});

const requestFrames = (socket: TestWebSocket) =>
  socket.sent
    .filter((frame): frame is string => typeof frame === "string")
    .map((frame) => decodeJson(frame) as { readonly _tag?: string })
    .filter((message) => message._tag === "Request")
    .map((message) => decodeRpcRequest(message));

const awaitRequest = Effect.fn("TestRpcSessionFactory.awaitRequest")(function* (
  socket: TestWebSocket,
  index = 0,
) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const request = requestFrames(socket)[index];
    if (request) return request;
    yield* Effect.yieldNow;
  }
  return yield* Effect.die(new Error("Expected the RPC protocol to send a request."));
});

const awaitBinaryFrame = Effect.fn("TestRpcSessionFactory.awaitBinaryFrame")(function* (
  socket: TestWebSocket,
  index: number,
) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const frame = socket.sent[index];
    if (frame instanceof Uint8Array) return frame;
    yield* Effect.yieldNow;
  }
  return yield* Effect.die(new Error(`Expected binary websocket frame ${String(index)}.`));
});

const completeInitialConfig = Effect.fn("TestRpcSessionFactory.completeInitialConfig")(function* (
  socket: TestWebSocket,
  config = SERVER_CONFIG,
) {
  const request = yield* awaitRequest(socket);
  expect(request).toMatchObject({
    _tag: "Request",
    tag: WS_METHODS.subscribeServerConfig,
    payload: {},
  });
  socket.serverMessage(
    encodeJson({
      _tag: "Chunk",
      requestId: request.id,
      values: [{ version: 1, type: "snapshot", config: encodeServerConfig(config) }],
    }),
  );
  return request;
});

const completeEncryptedInitialConfig = Effect.fn(
  "TestRpcSessionFactory.completeEncryptedInitialConfig",
)(function* (
  socket: TestWebSocket,
  transport: { readonly receive: { decryptWithAd(ad: Uint8Array, data: Uint8Array): Uint8Array } },
  sendRecords: (body: unknown) => void,
  frameIndex: number,
) {
  const record = transport.receive.decryptWithAd(
    new Uint8Array(0),
    yield* awaitBinaryFrame(socket, frameIndex),
  );
  const request = decodeRpcRequest(decodeJson(new TextDecoder().decode(record.subarray(1))));
  expect(request.tag).toBe(WS_METHODS.subscribeServerConfig);
  sendRecords({
    _tag: "Chunk",
    requestId: request.id,
    values: [{ version: 1, type: "snapshot", config: encodeServerConfig(SERVER_CONFIG) }],
  });
});

describe("RpcSessionFactory", () => {
  it.effect("exposes a plain socket connection before the first settings snapshot is ready", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const connected = yield* Effect.forkChild(session.connected);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        expect(connected.pollUnsafe()).toBeUndefined();
        socket.open();
        yield* Fiber.join(connected);
        expect(ready.pollUnsafe()).toBeUndefined();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
      }),
    ),
  );

  it.effect("fails connected when the socket closes before opening", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const connected = yield* Effect.forkChild(Effect.flip(session.connected));
        const socket = yield* awaitSocket(sockets);
        socket.close(1006);
        expect(yield* Fiber.join(connected)).toMatchObject({
          _tag: "ConnectionTransientError",
          reason: "transport",
          detail: "Could not establish a WebSocket connection.",
        });
      }),
    ),
  );

  it.effect("exposes an encrypted connection only after in-channel authentication", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
        const responder = createNkResponder({ staticPrivateKey: staticPrivate });
        const hostKey = Buffer.from(derivePublicKey(staticPrivate)).toString("base64url");
        const session = yield* factory.connect({
          ...PREPARED,
          socketUrl: "wss://environment.example.test/ws-e2ee",
          e2ee: { hostKey, auth: { kind: "bearer", credential: "stored-secret" } },
        });
        const connected = yield* Effect.forkChild(session.connected);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        responder.readMessageA(yield* awaitBinaryFrame(socket, 0));
        expect(connected.pollUnsafe()).toBeUndefined();
        socket.serverMessage(responder.writeMessageB(new Uint8Array(0)));
        const transport = responder.split();
        transport.receive.decryptWithAd(new Uint8Array(0), yield* awaitBinaryFrame(socket, 1));
        expect(connected.pollUnsafe()).toBeUndefined();
        const sendRecords = (body: unknown) => {
          for (const record of splitIntoRecords(new TextEncoder().encode(encodeJson(body)))) {
            socket.serverMessage(transport.send.encryptWithAd(new Uint8Array(0), record));
          }
        };
        sendRecords({ type: "e2ee_authenticated" });
        yield* Fiber.join(connected);
        expect(ready.pollUnsafe()).toBeUndefined();
        yield* completeEncryptedInitialConfig(socket, transport, sendRecords, 2);
        yield* Fiber.join(ready);
      }),
    ),
  );

  it.effect("starts ordered frames through the real session before earlier replies arrive", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket, {
          ...SERVER_CONFIG,
          environment: {
            ...SERVER_CONFIG.environment,
            capabilities: { ...SERVER_CONFIG.environment.capabilities, terminalOrderedInput: true },
          },
        });
        yield* Fiber.join(ready);
        const binding = createOrderedTerminalInputBinding(
          session,
          "environment-1",
          { threadId: ThreadId.make("thread"), terminalId: "terminal" },
          yield* Effect.context(),
        );
        const preparation = yield* Effect.forkChild(binding.prepare);
        const begin = yield* awaitRequest(socket, 1);
        expect(begin).toMatchObject({
          tag: WS_METHODS.terminalBeginInput,
          payload: { attachmentSequence: 0 },
        });
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: begin.id,
            exit: { _tag: "Success", value: { inputId: "lease" } },
          }),
        );
        yield* Fiber.join(preparation);
        const first = yield* Effect.forkChild(binding.write("a"));
        const a = yield* awaitRequest(socket, 2);
        const second = yield* Effect.forkChild(binding.write("b"));
        const b = yield* awaitRequest(socket, 3);
        expect(a).toMatchObject({
          tag: WS_METHODS.terminalWriteInput,
          payload: { inputId: "lease", sequence: 0, data: "a" },
        });
        expect(b).toMatchObject({
          tag: WS_METHODS.terminalWriteInput,
          payload: { inputId: "lease", sequence: 1, data: "b" },
        });
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: b.id,
            exit: { _tag: "Success", value: { inputId: "lease", sequence: 1 } },
          }),
        );
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: a.id,
            exit: { _tag: "Success", value: { inputId: "lease", sequence: 0 } },
          }),
        );
        yield* Fiber.join(first);
        yield* Fiber.join(second);
        binding.dispose();
        socket.close();
      }),
    ),
  );

  it.effect("fences a lost ordered reply at the real session seam without replay", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket, {
          ...SERVER_CONFIG,
          environment: {
            ...SERVER_CONFIG.environment,
            capabilities: { ...SERVER_CONFIG.environment.capabilities, terminalOrderedInput: true },
          },
        });
        yield* Fiber.join(ready);
        const binding = createOrderedTerminalInputBinding(
          session,
          "environment-1",
          { threadId: ThreadId.make("thread"), terminalId: "terminal" },
          yield* Effect.context(),
        );
        const preparation = yield* Effect.forkChild(binding.prepare);
        const begin = yield* awaitRequest(socket, 1);
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: begin.id,
            exit: { _tag: "Success", value: { inputId: "lease" } },
          }),
        );
        yield* Fiber.join(preparation);
        const writing = yield* Effect.forkChild(binding.write("uncertain").pipe(Effect.flip));
        const frame = yield* awaitRequest(socket, 2);
        expect(frame.tag).toBe(WS_METHODS.terminalWriteInput);
        yield* TestClock.adjust("15 seconds");
        const failure = yield* Fiber.join(writing);
        expect(failure._tag).toBe("TerminalInputError");
        yield* binding.write("do not replay").pipe(Effect.flip);
        const requests = socket.sent
          .filter((value): value is string => typeof value === "string")
          .map((value) => JSON.parse(value) as { tag?: string });
        expect(
          requests.filter((request) => request.tag === WS_METHODS.terminalWriteInput),
        ).toHaveLength(1);
        expect(requests.some((request) => request.tag === WS_METHODS.terminalWrite)).toBe(false);
        socket.close();
      }),
    ),
  );

  it.effect("owns one scoped websocket attempt and exposes readiness and closure", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const session = yield* factory.connect(PREPARED);
      const readyFiber = yield* Effect.forkChild(session.ready);
      const socket = yield* awaitSocket(sockets);

      expect(socket.url).toBe(PREPARED.socketUrl);
      socket.open();
      yield* completeInitialConfig(socket);
      yield* Fiber.join(readyFiber);

      const config = yield* session.initialConfig;
      expect(config).toEqual(SERVER_CONFIG);
      const e2eeAuthenticated = yield* session.e2eeAuthenticated;
      expect(e2eeAuthenticated).toBeNull();
      expect(requestFrames(socket).map((request) => request.tag)).toEqual([
        WS_METHODS.subscribeServerConfig,
      ]);

      socket.close(1012, "service restart");
      const error = yield* Effect.flip(session.closed);

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error).toMatchObject({
        reason: "connection-closed",
        message: "The connection disconnected.",
      });
      yield* Effect.yieldNow;
      expect(sockets).toHaveLength(1);
    }),
  );

  it.effect("preserves an unknown-request server defect as an exact die reason", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const readyFiber = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(readyFiber);

        const confirmation = yield* Effect.forkChild(
          session.client[WS_METHODS.authConfirmPairing]({}).pipe(Effect.exit),
        );
        const request = yield* awaitRequest(socket, 1);
        expect(request).toMatchObject({
          _tag: "Request",
          tag: WS_METHODS.authConfirmPairing,
          payload: {},
        });
        socket.serverMessage(
          encodeJson({
            _tag: "Defect",
            defect: `Unknown request tag: ${WS_METHODS.authConfirmPairing}`,
          }),
        );

        const exit = yield* Fiber.join(confirmation);
        expect(exit._tag).toBe("Failure");
        if (exit._tag !== "Failure") return;
        const { cause } = exit;
        expect(cause.reasons).toHaveLength(1);
        const reason = cause.reasons[0]!;
        expect(Cause.isDieReason(reason)).toBe(true);
        if (Cause.isDieReason(reason)) {
          expect(reason.defect).toBe(`Unknown request tag: ${WS_METHODS.authConfirmPairing}`);
        }
      }),
    ),
  );

  // Why clients gate the profiler RPCs on the `profiler` capability: an older server answers an
  // unknown method with a connection-level defect, which fails every request in flight and
  // silences the shared config stream, while the transport stays open.
  it.effect("an unknown-request defect fails other requests and silences the config stream", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const readyFiber = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        const configRequest = yield* completeInitialConfig(socket);
        yield* Fiber.join(readyFiber);

        const config = yield* Effect.forkChild(
          session.client[WS_METHODS.subscribeServerConfig]({}).pipe(
            Stream.take(2),
            Stream.runCollect,
          ),
        );
        const other = yield* Effect.forkChild(
          session.client[WS_METHODS.serverGetTraceDiagnostics]({}).pipe(Effect.exit),
        );
        const profiler = yield* Effect.forkChild(
          session.client[WS_METHODS.serverGetProfilerStatus]({}).pipe(Effect.exit),
        );
        yield* awaitRequest(socket, 2);
        socket.serverMessage(
          encodeJson({
            _tag: "Defect",
            defect: `Unknown request tag: ${WS_METHODS.serverGetProfilerStatus}`,
          }),
        );

        for (const exit of [yield* Fiber.join(other), yield* Fiber.join(profiler)]) {
          expect(exit._tag).toBe("Failure");
          if (exit._tag === "Failure") {
            expect(exit.cause.reasons.some(Cause.isDieReason)).toBe(true);
          }
        }
        // A later settings change on the original config stream never reaches a subscriber.
        socket.serverMessage(
          encodeJson({
            _tag: "Chunk",
            requestId: configRequest.id,
            values: [
              {
                version: 1,
                type: "settingsUpdated",
                payload: { settings: encodeServerSettings(DEFAULT_SERVER_SETTINGS) },
              },
            ],
          }),
        );
        for (let index = 0; index < 50; index += 1) yield* Effect.yieldNow;
        expect(config.pollUnsafe()).toBeUndefined();
        // And the session still looks connected.
        const closed = yield* Effect.forkChild(session.closed);
        for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
        expect(closed.pollUnsafe()).toBeUndefined();
      }),
    ),
  );

  it.effect("starts host-key sessions with a binary Noise NK message A", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const hostKey = Buffer.from(
        derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
      ).toString("base64url");

      yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect({
            ...PREPARED,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            e2ee: {
              hostKey,
              auth: { kind: "bearer", credential: "stored-secret" },
            },
          });
          const readyFiber = yield* Effect.forkChild(session.ready);
          const socket = yield* awaitSocket(sockets);
          expect(socket.url).toBe("wss://environment.example.test/ws-e2ee");
          expect(socket.binaryType).toBe("arraybuffer");

          socket.open();
          for (let attempt = 0; attempt < 100 && socket.sent.length === 0; attempt += 1) {
            yield* Effect.yieldNow;
          }
          expect(socket.sent[0]).toBeInstanceOf(Uint8Array);
          expect(socket.sent[0]).toHaveLength(48);
          yield* Fiber.interrupt(readyFiber);
        }),
      );
    }),
  );

  it.effect("blocks a session when the pinned host identity is rejected", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const hostKey = Buffer.from(
        derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
      ).toString("base64url");

      const error = yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect({
            ...PREPARED,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            e2ee: { hostKey, auth: { kind: "bearer", credential: "stored-secret" } },
          });
          const readyFiber = yield* Effect.forkChild(Effect.flip(session.ready));
          const socket = yield* awaitSocket(sockets);
          socket.open();
          yield* awaitBinaryFrame(socket, 0);
          socket.close(4403, "host identity mismatch");
          return yield* Fiber.join(readyFiber);
        }),
      );

      expect(error).toBeInstanceOf(ConnectionBlockedError);
      expect(error).toMatchObject({ reason: "host-identity" });
    }),
  );

  it.effect("blocks a session when in-channel bearer authentication is rejected", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
      const responder = createNkResponder({ staticPrivateKey: staticPrivate });
      const hostKey = Buffer.from(derivePublicKey(staticPrivate)).toString("base64url");

      const error = yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect({
            ...PREPARED,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            e2ee: { hostKey, auth: { kind: "bearer", credential: "stored-secret" } },
          });
          const readyFiber = yield* Effect.forkChild(Effect.flip(session.ready));
          const socket = yield* awaitSocket(sockets);
          socket.open();
          responder.readMessageA(yield* awaitBinaryFrame(socket, 0));
          socket.serverMessage(responder.writeMessageB(new Uint8Array(0)));
          const transport = responder.split();
          const authRecord = transport.receive.decryptWithAd(
            new Uint8Array(0),
            yield* awaitBinaryFrame(socket, 1),
          );
          expect(new TextDecoder().decode(authRecord.subarray(1))).toContain("stored-secret");
          for (const record of splitIntoRecords(
            new TextEncoder().encode(encodeJson({ type: "e2ee_error", code: "unauthorized" })),
          )) {
            socket.serverMessage(transport.send.encryptWithAd(new Uint8Array(0), record));
          }
          return yield* Fiber.join(readyFiber);
        }),
      );

      expect(error).toBeInstanceOf(ConnectionBlockedError);
      expect(error).toMatchObject({ reason: "authentication" });
    }),
  );

  it.effect("classifies a stalled encrypted handshake as a transient timeout", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const hostKey = Buffer.from(
        derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
      ).toString("base64url");

      const error = yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect({
            ...PREPARED,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            e2ee: { hostKey, auth: { kind: "bearer", credential: "stored-secret" } },
          });
          const readyFiber = yield* Effect.forkChild(Effect.flip(session.ready));
          const socket = yield* awaitSocket(sockets);
          socket.open();
          yield* awaitBinaryFrame(socket, 0);
          yield* TestClock.adjust("10 seconds");
          return yield* Fiber.join(readyFiber);
        }),
      );

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error).toMatchObject({ reason: "timeout" });
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("closes the websocket when the session scope is released", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();

      yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect(PREPARED);
          const readyFiber = yield* Effect.forkChild(session.ready);
          const socket = yield* awaitSocket(sockets);
          socket.open();
          yield* completeInitialConfig(socket);
          yield* Fiber.join(readyFiber);
        }),
      );

      expect(sockets[0]?.readyState).toBe(TestWebSocket.CLOSED);
    }),
  );

  it.effect("fails readiness when the websocket never opens", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();

      const error = yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect(PREPARED);
          const readyFiber = yield* Effect.forkChild(Effect.flip(session.ready));
          yield* awaitSocket(sockets);

          yield* TestClock.adjust("15 seconds");
          return yield* Fiber.join(readyFiber);
        }),
      );

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error).toMatchObject({
        reason: "transport",
        message: "Could not establish a WebSocket connection.",
      });
      expect(sockets[0]?.readyState).toBe(TestWebSocket.CLOSED);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("sends an RPC Ping only after ten seconds without inbound data", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        yield* TestClock.adjust("9 seconds");
        socket.serverMessage(encodeJson({ _tag: "Pong" }));
        yield* TestClock.adjust("9 seconds");
        expect(pingsSent(socket)).toBe(0);
        yield* TestClock.adjust("1 second");
        expect(pingsSent(socket)).toBe(1);
        expect(socket.readyState).toBe(TestWebSocket.OPEN);
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("closes with 4408 after thirty seconds without inbound data", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        yield* TestClock.adjust("29 seconds");
        expect(socket.readyState).toBe(TestWebSocket.OPEN);
        expect(pingsSent(socket)).toBe(2);
        yield* TestClock.adjust("1 second");
        const error = yield* Effect.flip(session.closed);
        expect(socket.closeCode).toBe(4408);
        expect(socket.closeReason).toBe("liveness timeout");
        expect(error).toBeInstanceOf(ConnectionTransientError);
        expect(error).toMatchObject({ reason: "liveness-timeout" });
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("keeps a connection while inbound messages keep arriving", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        for (let elapsed = 0; elapsed < 64; elapsed += 8) {
          yield* TestClock.adjust("8 seconds");
          socket.serverMessage(encodeJson({ _tag: "Pong" }));
        }
        expect(socket.readyState).toBe(TestWebSocket.OPEN);
        expect(pingsSent(socket)).toBe(0);
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("counts E2EE records as activity before a message is complete", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
      const responder = createNkResponder({ staticPrivateKey: staticPrivate });
      const hostKey = Buffer.from(derivePublicKey(staticPrivate)).toString("base64url");

      yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect({
            ...PREPARED,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            e2ee: { hostKey, auth: { kind: "bearer", credential: "stored-secret" } },
          });
          const ready = yield* Effect.forkChild(session.ready);
          const socket = yield* awaitSocket(sockets);
          socket.open();
          responder.readMessageA(yield* awaitBinaryFrame(socket, 0));
          socket.serverMessage(responder.writeMessageB(new Uint8Array(0)));
          const transport = responder.split();
          const sendRecords = (body: unknown) => {
            for (const record of splitIntoRecords(new TextEncoder().encode(encodeJson(body)))) {
              socket.serverMessage(transport.send.encryptWithAd(new Uint8Array(0), record));
            }
          };
          transport.receive.decryptWithAd(new Uint8Array(0), yield* awaitBinaryFrame(socket, 1));
          sendRecords({ type: "e2ee_authenticated" });
          yield* completeEncryptedInitialConfig(socket, transport, sendRecords, 2);
          yield* Fiber.join(ready);

          // Forty seconds of continuation records for a message that never completes.
          for (let elapsed = 0; elapsed < 40; elapsed += 8) {
            yield* TestClock.adjust("8 seconds");
            socket.serverMessage(
              transport.send.encryptWithAd(new Uint8Array(0), Uint8Array.of(0x01, 0x20)),
            );
          }
          expect(socket.readyState).toBe(TestWebSocket.OPEN);
          yield* TestClock.adjust("30 seconds");
          expect(socket.closeCode).toBe(4408);
        }),
      );
    }).pipe(withFixedRandom),
  );
  for (const [label, trigger, reason] of [
    ["a liveness timeout", "liveness", "liveness-timeout"],
    ["a close frame from the server", 1012, "connection-closed"],
    ["an abnormal closure", 1006, "connection-lost"],
  ] as const) {
    it.effect(
      `fails readiness with the classified reason after ${label} while the config is pending`,
      () =>
        Effect.scoped(
          Effect.gen(function* () {
            const { factory, sockets } = yield* makeFactory();
            const session = yield* factory.connect(PREPARED);
            const ready = yield* Effect.forkChild(Effect.flip(session.ready));
            const socket = yield* awaitSocket(sockets);
            socket.open();
            yield* awaitRequest(socket);
            if (trigger === "liveness") {
              yield* TestClock.adjust("30 seconds");
            } else {
              socket.close(trigger, "");
            }
            const error = yield* Fiber.join(ready);
            expect(error).toBeInstanceOf(ConnectionTransientError);
            expect(error).toMatchObject({ reason, message: "The connection disconnected." });
          }),
        ).pipe(withFixedRandom),
    );
  }

  it.effect("fails a probe with the classified reason when the socket ends during it", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const probing = yield* Effect.forkChild(Effect.flip(session.probe));
        for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
        expect(probing.pollUnsafe()).toBeUndefined();
        socket.close(1006, "");
        const error = yield* Fiber.join(probing);
        expect(error).toBeInstanceOf(ConnectionTransientError);
        expect(error).toMatchObject({ reason: "connection-lost" });
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("still fails a pending request with RpcClientError when the socket ends", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const pending = yield* Effect.forkChild(
          Effect.flip(session.client[WS_METHODS.serverGetSettings]({})),
        );
        yield* awaitRequest(socket, 1);
        socket.close(1006, "");
        const error = yield* Fiber.join(pending);
        // The clone re-attach classifies this exact failure as a transport loss.
        expect(error._tag).toBe("RpcClientError");
        expect((yield* Effect.flip(session.closed)).reason).toBe("connection-lost");
      }),
    ).pipe(withFixedRandom),
  );
  it.effect("offers the chunked subprotocols, compression first, on plain sockets only", () =>
    Effect.gen(function* () {
      const { factory, sockets } = yield* makeFactory();
      yield* Effect.scoped(
        Effect.gen(function* () {
          const session = yield* factory.connect(PREPARED);
          yield* Effect.forkChild(session.ready);
          const socket = yield* awaitSocket(sockets);
          expect(socket.protocols).toEqual([
            "bibcode.rpc.chunked-deflate.v1",
            "bibcode.rpc.chunked.v1",
          ]);
          expect(socket.binaryType).toBe("arraybuffer");
        }),
      );
    }),
  );

  it.effect("sends read subscriptions while configuring and holds their data until verified", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* awaitRequest(socket, 0);
        const gate = yield* RpcSession.makeVerifiedSessionGate(session);

        const servers = yield* Effect.forkChild(
          Stream.runHead(gate.session.client[WS_METHODS.subscribeDiscoveredLocalServers]({})),
        );
        const settings = yield* Effect.forkChild(
          gate.session.client[WS_METHODS.serverGetSettings]({}),
        );
        // The subscription leaves before the config arrives: no round trip of its own after it.
        const subscription = yield* awaitRequest(socket, 1);
        expect(subscription.tag).toBe(WS_METHODS.subscribeDiscoveredLocalServers);
        const list = { servers: [], scannedAt: "2026-10-17T00:00:00.000Z" };
        socket.serverMessage(
          encodeJson({ _tag: "Chunk", requestId: subscription.id, values: [list] }),
        );
        for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
        // Unverified data stays held, and the unary call has not left.
        expect(servers.pollUnsafe()).toBeUndefined();
        expect(requestFrames(socket)).toHaveLength(2);

        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
        yield* gate.open;
        expect(yield* Fiber.join(servers)).toEqual(Option.some(list));
        const settingsRequest = yield* awaitRequest(socket, 2);
        expect(settingsRequest.tag).toBe(WS_METHODS.serverGetSettings);
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: settingsRequest.id,
            exit: { _tag: "Success", value: encodeServerSettings(DEFAULT_SERVER_SETTINGS) },
          }),
        );
        expect(yield* Fiber.join(settings)).toEqual(DEFAULT_SERVER_SETTINGS);
      }),
    ),
  );

  it.effect("releases nothing from an attempt that ends unverified", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* awaitRequest(socket, 0);
        const gate = yield* RpcSession.makeVerifiedSessionGate(session);
        const servers = yield* Effect.forkChild(
          Effect.flip(
            Stream.runHead(gate.session.client[WS_METHODS.subscribeDiscoveredLocalServers]({})),
          ),
        );
        const subscription = yield* awaitRequest(socket, 1);
        socket.serverMessage(
          encodeJson({
            _tag: "Chunk",
            requestId: subscription.id,
            values: [{ servers: [], scannedAt: "2026-10-17T00:00:00.000Z" }],
          }),
        );
        const settings = yield* Effect.forkChild(
          Effect.flip(gate.session.client[WS_METHODS.serverGetSettings]({})),
        );
        const config = yield* Effect.forkChild(gate.session.initialConfig);
        yield* completeInitialConfig(socket);
        yield* gate.close;
        // A one-shot call fails; the subscription and the config read wait until their
        // follower moves on, and deliver nothing from the unverified server.
        expect((yield* Fiber.join(settings))._tag).toBe("RpcClientError");
        for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
        expect(servers.pollUnsafe()).toBeUndefined();
        expect(config.pollUnsafe()).toBeUndefined();
        // The unary call never left.
        expect(requestFrames(socket).map((request) => request.tag)).not.toContain(
          WS_METHODS.serverGetSettings,
        );
      }),
    ),
  );

  it.effect("reassembles records when the server selected the subprotocol", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.protocol = "bibcode.rpc.chunked.v1";
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const settings = yield* Effect.forkChild(session.client[WS_METHODS.serverGetSettings]({}));
        const request = yield* awaitRequest(socket, 1);
        const body = new TextEncoder().encode(
          encodeJson({
            _tag: "Exit",
            requestId: request.id,
            exit: { _tag: "Success", value: encodeServerSettings(DEFAULT_SERVER_SETTINGS) },
          }),
        );
        const record = (flag: number, bytes: Uint8Array) => {
          const out = new Uint8Array(1 + bytes.length);
          out[0] = flag;
          out.set(bytes, 1);
          return out;
        };
        socket.serverMessage(record(0x01, body.subarray(0, 10)));
        socket.serverMessage(record(0x02, new TextEncoder().encode(encodeJson({ _tag: "Pong" }))));
        socket.serverMessage(record(0x00, body.subarray(10)));
        expect(yield* Fiber.join(settings)).toEqual(DEFAULT_SERVER_SETTINGS);
      }),
    ),
  );

  it.effect("inflates compressed records when the server selected compression", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.protocol = "bibcode.rpc.chunked-deflate.v1";
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const settings = yield* Effect.forkChild(session.client[WS_METHODS.serverGetSettings]({}));
        const request = yield* awaitRequest(socket, 1);
        const body = new TextEncoder().encode(
          encodeJson({
            _tag: "Exit",
            requestId: request.id,
            exit: { _tag: "Success", value: encodeServerSettings(DEFAULT_SERVER_SETTINGS) },
          }),
        );
        const deflated = NodeZlib.deflateRawSync(body);
        const record = new Uint8Array(1 + 4 + deflated.length);
        record[0] = 0x10;
        new DataView(record.buffer).setUint32(1, body.length);
        record.set(deflated, 5);
        socket.serverMessage(record);
        expect(yield* Fiber.join(settings)).toEqual(DEFAULT_SERVER_SETTINGS);
      }),
    ),
  );

  it.effect("keeps binary frames whole when the server did not select the subprotocol", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const settings = yield* Effect.forkChild(session.client[WS_METHODS.serverGetSettings]({}));
        const request = yield* awaitRequest(socket, 1);
        socket.serverMessage(
          new TextEncoder().encode(
            encodeJson({
              _tag: "Exit",
              requestId: request.id,
              exit: { _tag: "Success", value: encodeServerSettings(DEFAULT_SERVER_SETTINGS) },
            }),
          ),
        );
        expect(yield* Fiber.join(settings)).toEqual(DEFAULT_SERVER_SETTINGS);
      }),
    ),
  );
  it.effect("fails only the request whose response is too large", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const settings = yield* Effect.forkChild(
          Effect.flip(session.client[WS_METHODS.serverGetSettings]({})),
        );
        const request = yield* awaitRequest(socket, 1);
        socket.serverMessage(
          encodeJson({
            _tag: "Exit",
            requestId: request.id,
            exit: {
              _tag: "Failure",
              cause: [
                {
                  _tag: "Fail",
                  error: {
                    _tag: "RpcResponseTooLargeError",
                    method: WS_METHODS.serverGetSettings,
                    bytes: 70_000_000,
                    limitBytes: 67_108_864,
                  },
                },
              ],
            },
          }),
        );
        const error = yield* Fiber.join(settings);
        expect(error).toBeInstanceOf(RpcResponseTooLargeError);
        expect(error.message).toBe("This result is too large to send (66.8 MiB; limit 64 MiB).");
        expect(socket.readyState).toBe(TestWebSocket.OPEN);
      }),
    ),
  );
  it.effect("becomes ready from the first config snapshot without calling server.getConfig", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
        expect(yield* session.initialConfig).toEqual(SERVER_CONFIG);
        expect(requestFrames(socket).map((request) => request.tag)).toEqual([
          WS_METHODS.subscribeServerConfig,
        ]);
      }),
    ),
  );

  for (const [error, expected] of [
    [
      { _tag: "UpdateMaintenanceActiveError", message: "An update is being installed." },
      {
        _tag: "ConnectionTransientError",
        reason: "remote-unavailable",
        detail: "An update is being installed.",
      },
    ],
    [
      {
        _tag: "KeybindingsConfigParseError",
        configPath: "/home/dev/keybindings.json",
        detail: "Unexpected token.",
      },
      {
        _tag: "ConnectionTransientError",
        reason: "remote-unavailable",
        detail:
          "Unable to parse keybindings config at /home/dev/keybindings.json: Unexpected token.",
      },
    ],
    [
      {
        _tag: "ServerSettingsError",
        settingsPath: "/home/dev/settings.json",
        operation: "read-file",
        cause: "unreadable",
      },
      {
        _tag: "ConnectionTransientError",
        reason: "remote-unavailable",
        detail: "Server settings read-file failed at /home/dev/settings.json.",
      },
    ],
    [
      {
        _tag: "RpcResponseTooLargeError",
        method: WS_METHODS.subscribeServerConfig,
        bytes: 70_000_000,
        limitBytes: 67_108_864,
      },
      {
        _tag: "ConnectionTransientError",
        reason: "remote-unavailable",
        detail: "This result is too large to send (66.8 MiB; limit 64 MiB).",
      },
    ],
    [
      {
        _tag: "EnvironmentAuthorizationError",
        message: "This client may not read the configuration.",
        requiredScope: "orchestration:read",
      },
      {
        _tag: "ConnectionBlockedError",
        reason: "permission",
        detail: "This client may not read the configuration.",
      },
    ],
  ] as const) {
    it.effect(`fails readiness on a config ${error._tag} as ${expected.reason}`, () =>
      Effect.scoped(
        Effect.gen(function* () {
          const { factory, sockets } = yield* makeFactory();
          const session = yield* factory.connect(PREPARED);
          const ready = yield* Effect.forkChild(Effect.flip(session.ready));
          const socket = yield* awaitSocket(sockets);
          socket.open();
          const request = yield* awaitRequest(socket);
          socket.serverMessage(
            encodeJson({
              _tag: "Exit",
              requestId: request.id,
              exit: { _tag: "Failure", cause: [{ _tag: "Fail", error }] },
            }),
          );
          expect(yield* Fiber.join(ready)).toMatchObject(expected);
        }),
      ),
    );
  }

  it.effect("serves later config subscriptions from the one server stream", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        const configRequest = yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const first = yield* session.client[WS_METHODS.subscribeServerConfig]({}).pipe(
          Stream.take(1),
          Stream.runCollect,
        );
        expect(Array.from(first)[0]).toMatchObject({ type: "snapshot" });
        const subscriber = yield* Effect.forkChild(
          session.client[WS_METHODS.subscribeServerConfig]({}).pipe(
            Stream.take(2),
            Stream.runCollect,
          ),
        );
        for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
        socket.serverMessage(
          encodeJson({
            _tag: "Chunk",
            requestId: configRequest.id,
            values: [
              { version: 1, type: "keybindingsUpdated", payload: { keybindings: [], issues: [] } },
            ],
          }),
        );
        const events = Array.from(yield* Fiber.join(subscriber));
        expect(events.map((event) => event.type)).toEqual(["snapshot", "keybindingsUpdated"]);
        expect(
          requestFrames(socket).filter(
            (request) => request.tag === WS_METHODS.subscribeServerConfig,
          ),
        ).toHaveLength(1);
      }),
    ),
  );

  it.effect("probes with an RPC Ping and succeeds at the next inbound message", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);

        const probing = yield* Effect.forkChild(session.probe);
        for (let attempt = 0; attempt < 100 && pingsSent(socket) === 0; attempt += 1) {
          yield* Effect.yieldNow;
        }
        expect(pingsSent(socket)).toBe(1);
        expect(probing.pollUnsafe()).toBeUndefined();
        socket.serverMessage(encodeJson({ _tag: "Pong" }));
        yield* Fiber.join(probing);
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("fails a health probe when the connection drops while it waits for data", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
        const probing = yield* Effect.forkChild(Effect.flip(session.probe));
        for (let i = 0; i < 100 && pingsSent(socket) === 0; i += 1) yield* Effect.yieldNow;
        expect(pingsSent(socket)).toBe(1);
        expect(probing.pollUnsafe()).toBeUndefined();
        socket.close(1006, "");
        const error = yield* Fiber.join(probing);
        expect(error).toBeInstanceOf(ConnectionTransientError);
        expect(error.reason).toBe("connection-lost");
      }),
    ).pipe(withFixedRandom),
  );

  it.effect("fails the probe once the socket has closed", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
        socket.close(1000);
        yield* Effect.flip(session.closed);
        const error = yield* Effect.flip(session.probe);
        expect(error).toBeInstanceOf(ConnectionTransientError);
      }),
    ),
  );

  it.effect("reports an abnormal closure as a lost connection", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { factory, sockets } = yield* makeFactory();
        const session = yield* factory.connect(PREPARED);
        const ready = yield* Effect.forkChild(session.ready);
        const socket = yield* awaitSocket(sockets);
        socket.open();
        yield* completeInitialConfig(socket);
        yield* Fiber.join(ready);
        socket.close(1006, "");
        const error = yield* Effect.flip(session.closed);
        expect(error).toMatchObject({
          reason: "connection-lost",
          message: "The connection disconnected.",
        });
      }),
    ),
  );
});
