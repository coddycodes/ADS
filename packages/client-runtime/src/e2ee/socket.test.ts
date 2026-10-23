// @effect-diagnostics nodeBuiltinImport:off - The scripted server compresses replies with zlib.
import * as NodeZlib from "node:zlib";

import type { E2eeAuthenticatedMessage } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as FiberSet from "effect/FiberSet";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Socket from "effect/socket/Socket";
import { vi } from "vite-plus/test";

import { RecordAssembler, splitIntoRecords } from "./frame.ts";
import { makeRawSocket, runRaw, runString } from "./rawSocket.ts";
import { createNkResponder, derivePublicKey, type NkTransport } from "./noise.ts";
import { e2eeFailureOf, E2eeProtocolError, makeE2eeSocket } from "./socket.ts";

const encryptProbe = vi.hoisted(() => ({ calls: 0 }));

vi.mock("./noise.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./noise.ts")>();
  return {
    ...actual,
    createNkInitiator(options: Parameters<typeof actual.createNkInitiator>[0]) {
      const initiator = actual.createNkInitiator(options);
      return {
        ...initiator,
        split() {
          const transport = initiator.split();
          return {
            ...transport,
            send: {
              encryptWithAd(ad: Uint8Array, plaintext: Uint8Array) {
                encryptProbe.calls += 1;
                return transport.send.encryptWithAd(ad, plaintext);
              },
              decryptWithAd(ad: Uint8Array, ciphertext: Uint8Array) {
                return transport.send.decryptWithAd(ad, ciphertext);
              },
            },
          };
        },
      };
    },
  };
});

const decodeJson = Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Unknown));
const encodeJson = Schema.encodeUnknownSync(Schema.fromJsonString(Schema.Unknown));

const makeScriptedInnerSocket = (
  onFrame: (
    frame: Uint8Array,
    emit: (frame: Uint8Array) => void,
    close: (code?: number) => void,
  ) => void,
  beforeWrite?: (frame: Uint8Array) => Effect.Effect<void>,
  options?: { readonly frameType?: "uint8array" | "arraybuffer" },
): Socket.Socket => {
  let deliver: ((frame: Uint8Array) => void) | null = null;
  // Effect's WebSocket socket passes a browser frame through `runRaw`
  // untouched, so under binaryType "arraybuffer" the handler receives an
  // ArrayBuffer rather than a Uint8Array.
  const asDelivered = (frame: Uint8Array): Uint8Array =>
    options?.frameType === "arraybuffer"
      ? (frame.buffer.slice(
          frame.byteOffset,
          frame.byteOffset + frame.byteLength,
        ) as unknown as Uint8Array)
      : frame;
  let finish: ((code?: number) => void) | null = null;
  const pending: Array<Uint8Array> = [];
  let closed: { code?: number } | null = null;
  const emit = (frame: Uint8Array): void => {
    if (deliver === null) pending.push(frame);
    else deliver(frame);
  };
  const close = (code?: number): void => {
    queueMicrotask(() => {
      closed = code === undefined ? {} : { code };
      if (finish !== null) finish(code);
    });
  };

  const runRaw = <A, E, R>(
    handler: (data: string | Uint8Array) => Effect.Effect<A, E, R> | void,
    options?: { readonly onOpen?: Effect.Effect<void> | undefined },
  ): Effect.Effect<void, Socket.SocketError | E, R> =>
    Effect.scopedWith((scope) =>
      Effect.gen(function* () {
        const fiberSet = yield* FiberSet.make<unknown, Socket.SocketError | E>().pipe(
          Scope.provide(scope),
        );
        const run = yield* FiberSet.runtime(fiberSet)<R>();
        deliver = (frame) => {
          const result = handler(asDelivered(frame));
          if (Effect.isEffect(result)) run(result);
        };
        finish = (code) =>
          Deferred.doneUnsafe(
            fiberSet.deferred,
            code === undefined || code === 1000
              ? Effect.void
              : Effect.fail(
                  new Socket.SocketError({
                    reason: new Socket.SocketCloseError({ code, closeReason: "" }),
                  }),
                ),
          );
        for (const frame of pending.splice(0)) deliver(frame);
        const closedSnapshot = closed;
        if (closedSnapshot !== null) finish(closedSnapshot.code);
        else if (options?.onOpen) yield* options.onOpen;
        return yield* FiberSet.join(fiberSet);
      }),
    );

  const write = (chunk: Uint8Array | string | Socket.CloseEvent) =>
    Effect.suspend(() => {
      if (typeof chunk === "string" || chunk instanceof Uint8Array) {
        const bytes = typeof chunk === "string" ? new TextEncoder().encode(chunk) : chunk;
        return Effect.andThen(
          beforeWrite?.(bytes) ?? Effect.void,
          Effect.sync(() => onFrame(bytes, emit, close)),
        );
      } else {
        close();
        return Effect.void;
      }
    });

  return makeRawSocket({
    runRaw,
    writer: Effect.succeed({
      write,
      writeAll: (chunks) => Effect.forEach(chunks, write, { discard: true }),
    }),
  });
};

const findE2eeCause = (exit: Exit.Exit<unknown, unknown>): E2eeProtocolError | null => {
  if (exit._tag !== "Failure") return null;
  const failure = Cause.findErrorOption(exit.cause);
  return Option.isSome(failure) ? e2eeFailureOf(failure.value) : null;
};

const responderScript = (options?: {
  failAuth?: boolean;
  messageBPayload?: Uint8Array;
  confirmInterleave?: boolean;
  interleaveControl?: boolean;
  /** Confirm deflate-v1 and answer with a compressed echo, then a plain one. */
  deflateEcho?: { declaredBytes?: number; closeAfter?: boolean };
}) => {
  const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
  const hostKey = derivePublicKey(staticPrivate);
  const responder = createNkResponder({ staticPrivateKey: staticPrivate });
  const assembler = new RecordAssembler();
  let transport: NkTransport | null = null;
  const received: Array<string> = [];
  const currentTransport = (): NkTransport => {
    if (transport === null) throw new Error("transport is not ready");
    return transport;
  };
  const script = (
    frame: Uint8Array,
    emit: (frame: Uint8Array) => void,
    close: (code?: number) => void,
  ): void => {
    if (transport === null) {
      responder.readMessageA(frame);
      const messageB = responder.writeMessageB(options?.messageBPayload ?? new Uint8Array(0));
      transport = responder.split();
      emit(messageB);
      return;
    }
    const record = transport.receive.decryptWithAd(new Uint8Array(0), frame);
    const message = assembler.push(record);
    if (message === null) return;
    const text = new TextDecoder().decode(message);
    received.push(text);
    const reply = (body: object) => {
      for (const outRecord of splitIntoRecords(new TextEncoder().encode(encodeJson(body)))) {
        emit(currentTransport().send.encryptWithAd(new Uint8Array(0), outRecord));
      }
    };
    const decoded = decodeJson(text);
    const parsed = decoded as { type?: string; pairing?: string; bearer?: string };
    if (parsed.type === "e2ee_auth") {
      if (options?.failAuth) {
        reply({ type: "e2ee_error", code: "unauthorized" });
        close();
      } else if (parsed.pairing !== undefined) {
        reply({
          type: "e2ee_authenticated",
          credential: `minted-for-${parsed.pairing}`,
          environmentId: "env-1",
          storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
          pairingConfirmationRequired: true,
        });
      } else {
        reply({
          type: "e2ee_authenticated",
          ...(options?.confirmInterleave === true ? { features: ["interleave-v1"] } : {}),
          ...(options?.deflateEcho !== undefined
            ? { features: ["interleave-v1", "deflate-v1"] }
            : {}),
        });
      }
      return;
    }
    if (options?.interleaveControl === true) {
      const body = new TextEncoder().encode(encodeJson({ echoed: text.length }));
      const send = (bytes: Uint8Array) =>
        emit(currentTransport().send.encryptWithAd(new Uint8Array(0), bytes));
      send(Uint8Array.of(0x01, ...body.subarray(0, 4)));
      send(Uint8Array.of(0x02, ...new TextEncoder().encode(encodeJson({ control: true }))));
      send(Uint8Array.of(0x00, ...body.subarray(4)));
      return;
    }
    if (options?.deflateEcho !== undefined) {
      const body = new TextEncoder().encode(
        encodeJson({ echoed: text.length, padding: "x".repeat(64 * 1024) }),
      );
      const compressed = NodeZlib.deflateRawSync(body);
      const message = new Uint8Array(4 + compressed.length);
      new DataView(message.buffer).setUint32(0, options.deflateEcho.declaredBytes ?? body.length);
      message.set(compressed, 4);
      for (const outRecord of splitIntoRecords(message)) {
        outRecord[0]! |= 0x10;
        emit(currentTransport().send.encryptWithAd(new Uint8Array(0), outRecord));
      }
      reply({ after: true });
      if (options.deflateEcho.closeAfter === true) close();
      return;
    }
    reply({ echoed: text.length });
  };
  return { hostKey, script, received };
};

describe("makeE2eeSocket", () => {
  it.effect("offers interleave-v1 and deflate-v1 in the auth message", () =>
    Effect.gen(function* () {
      const responder = responderScript();
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runRaw(socket, () => undefined, {
          onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid),
        }),
      );
      yield* Deferred.await(opened);
      expect(decodeJson(responder.received[0]!)).toMatchObject({
        type: "e2ee_auth",
        bearer: "stored",
        features: ["interleave-v1", "deflate-v1"],
      });
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("delivers a mid-message control record once the server confirms interleave-v1", () =>
    Effect.gen(function* () {
      const responder = responderScript({ confirmInterleave: true, interleaveControl: true });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const received: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runRaw(
          socket,
          (data) => {
            received.push(typeof data === "string" ? data : new TextDecoder().decode(data));
          },
          { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      for (let attempt = 0; attempt < 100 && received.length < 2; attempt += 1) {
        yield* Effect.yieldNow;
      }
      expect(received.map((text) => decodeJson(text))).toEqual([{ control: true }, { echoed: 14 }]);
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("inflates a compressed message and keeps it ahead of the next plain one", () =>
    Effect.gen(function* () {
      const responder = responderScript({ deflateEcho: {} });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const received: Array<string> = [];
      const both = yield* Deferred.make<void>();
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runRaw(
          socket,
          (data) => {
            received.push(typeof data === "string" ? data : new TextDecoder().decode(data));
            if (received.length === 2) Deferred.doneUnsafe(both, Effect.void);
          },
          { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      yield* Deferred.await(both);
      expect(received.map((text) => decodeJson(text))).toEqual([
        { echoed: 14, padding: "x".repeat(64 * 1024) },
        { after: true },
      ]);
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("starts handler effects in arrival order across a compressed message", () =>
    Effect.gen(function* () {
      // The RPC client routes a reply inside the effect its handler returns, so the
      // effects must begin in arrival order, not only the handler calls.
      const responder = responderScript({ deflateEcho: {} });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const started: Array<string> = [];
      const both = yield* Deferred.make<void>();
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runRaw(
          socket,
          (data) => {
            const text = typeof data === "string" ? data : new TextDecoder().decode(data);
            const name =
              (decodeJson(text) as { after?: boolean }).after === true ? "plain" : "compressed";
            return Effect.sync(() => {
              started.push(name);
              if (started.length === 2) Deferred.doneUnsafe(both, Effect.void);
            });
          },
          { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      yield* Deferred.await(both);
      expect(started).toEqual(["compressed", "plain"]);
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("does not hold later messages behind a compressed message's slow handler", () =>
    Effect.gen(function* () {
      // A stream response's handler can wait on a full consumer queue; replies after it
      // must still reach their own handlers.
      const responder = responderScript({ deflateEcho: {} });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const backpressure = yield* Deferred.make<void>();
      const plainHandled = yield* Deferred.make<void>();
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runRaw(
          socket,
          (data) => {
            const text = typeof data === "string" ? data : new TextDecoder().decode(data);
            return (decodeJson(text) as { after?: boolean }).after === true
              ? Deferred.succeed(plainHandled, undefined)
              : Deferred.await(backpressure);
          },
          { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      yield* Deferred.await(plainHandled);
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("never delivers a message ahead of a compressed one cut off by a close", () =>
    Effect.gen(function* () {
      const responder = responderScript({ deflateEcho: { closeAfter: true } });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const received: Array<unknown> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.exit(
          runRaw(
            socket,
            (data) => {
              received.push(
                decodeJson(typeof data === "string" ? data : new TextDecoder().decode(data)),
              );
            },
            { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
          ),
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      yield* Fiber.join(fiber);
      // Whatever arrived before the close arrived in order.
      expect(received).toEqual(
        received.length === 0
          ? []
          : [{ echoed: 14, padding: "x".repeat(64 * 1024) }, { after: true }],
      );
    }),
  );

  it.effect("fails closed when a compressed message inflates past its declared size", () =>
    Effect.gen(function* () {
      const responder = responderScript({ deflateEcho: { declaredBytes: 1024 } });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const received: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.exit(
          runRaw(
            socket,
            (data) => {
              received.push(typeof data === "string" ? data : new TextDecoder().decode(data));
            },
            { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
          ),
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      const exit = yield* Fiber.join(fiber);
      expect(findE2eeCause(exit)?.reason).toBe("protocol");
      expect(received).toEqual([]);
    }),
  );

  it.effect("fails closed on a control record the server never confirmed", () =>
    Effect.gen(function* () {
      const responder = responderScript({ interleaveControl: true });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(responder.script), {
        hostKey: Buffer.from(responder.hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored" },
      });
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.exit(
          runRaw(socket, () => undefined, {
            onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid),
          }),
        ),
      );
      yield* Deferred.await(opened);
      const { write } = yield* Scope.provide(socket.writer, yield* Scope.make());
      yield* write('{"hello":true}');
      const exit = yield* Fiber.join(fiber);
      expect(findE2eeCause(exit)?.reason).toBe("protocol");
    }),
  );

  it.effect("fails closed when the transport delivers a non-binary frame", () =>
    Effect.gen(function* () {
      // A Blob here means the underlying WebSocket was left on binaryType
      // "blob": asynchronous conversion would reorder ciphertext against the
      // counter-based Noise nonce, so the channel must fail with a named
      // invariant instead of an intermittent decrypt error.
      const inner = makeRawSocket({
        runRaw: (handler, options) =>
          Effect.gen(function* () {
            if (options?.onOpen !== undefined) yield* options.onOpen;
            const result = handler(new Blob(["x"]) as unknown as Uint8Array);
            if (result !== undefined) yield* result;
            return yield* Effect.never;
          }),
        writer: Effect.succeed({ write: () => Effect.void, writeAll: () => Effect.void }),
      });
      // The socket opens first, as a real one does, so message A needs a valid
      // host key before the Blob frame arrives.
      const socket = makeE2eeSocket(inner, {
        hostKey: Buffer.from(derivePublicKey(crypto.getRandomValues(new Uint8Array(32)))).toString(
          "base64url",
        ),
        auth: { kind: "bearer", credential: "credential" },
      });

      const exit = yield* runRaw(socket, () => undefined).pipe(Effect.exit);
      const cause = findE2eeCause(exit);
      expect(cause?.reason).toBe("protocol");
      expect(cause?.message).toContain('binaryType "arraybuffer"');
    }),
  );

  it.live("handshakes, bootstraps in-channel, then delivers decrypted strings", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript();
      const authenticated: Array<E2eeAuthenticatedMessage> = [];
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "one-time-1" },
        onAuthenticated: (message) => {
          authenticated.push(message);
        },
      });
      const delivered: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.scoped(
          Effect.gen(function* () {
            const { write } = yield* socket.writer;
            const queuedWrite = yield* Effect.forkChild(write(encodeJson({ hello: true })), {
              startImmediately: true,
            });
            yield* Effect.forkChild(
              runString(
                socket,
                (text) => {
                  delivered.push(text);
                },
                { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
              ),
            );
            yield* Deferred.await(opened);
            expect(authenticated).toHaveLength(1);
            yield* Fiber.join(queuedWrite);
            yield* Effect.sleep("50 millis");
          }),
        ),
      );
      yield* Effect.sleep("200 millis");
      yield* Fiber.interrupt(fiber);
      expect(received[0]).toBe(
        encodeJson({
          type: "e2ee_auth",
          pairing: "one-time-1",
          features: ["interleave-v1", "deflate-v1"],
        }),
      );
      expect(received[1]).toBe(encodeJson({ hello: true }));
      expect(delivered).toEqual([encodeJson({ echoed: received[1]?.length })]);
      expect(authenticated[0]?.credential).toBe("minted-for-one-time-1");
      expect(authenticated[0]?.environmentId).toBe("env-1");
    }),
  );

  it.live("bearer form sends the stored credential", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript();
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored-1" },
      });
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        runString(socket, () => {}, {
          onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid),
        }),
      );
      yield* Deferred.await(opened);
      yield* Fiber.interrupt(fiber);
      expect(received[0]).toBe(
        encodeJson({
          type: "e2ee_auth",
          bearer: "stored-1",
          features: ["interleave-v1", "deflate-v1"],
        }),
      );
    }),
  );

  it.live("handshakes when the browser transport delivers ArrayBuffer frames", () =>
    Effect.gen(function* () {
      // Real WebSockets switched to binaryType "arraybuffer" deliver
      // ArrayBuffer frames, and Effect's raw path hands them over as-is. The
      // channel must treat them as binary, not fail closed as a non-binary
      // frame; that failure surfaced as "Server unreachable" on every desktop.
      const { hostKey, script, received } = responderScript();
      const socket = makeE2eeSocket(
        makeScriptedInnerSocket(script, undefined, { frameType: "arraybuffer" }),
        {
          hostKey: Buffer.from(hostKey).toString("base64url"),
          auth: { kind: "pairing", token: "one-time-2" },
        },
      );
      const delivered: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.scoped(
          Effect.gen(function* () {
            const { write } = yield* socket.writer;
            yield* Effect.forkChild(
              runString(
                socket,
                (text) => {
                  delivered.push(text);
                },
                { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
              ),
            );
            yield* Deferred.await(opened);
            yield* write(encodeJson({ hello: true }));
            yield* Effect.sleep("50 millis");
          }),
        ),
      );
      yield* Effect.sleep("200 millis");
      yield* Fiber.interrupt(fiber);
      expect(received[0]).toBe(
        encodeJson({
          type: "e2ee_auth",
          pairing: "one-time-2",
          features: ["interleave-v1", "deflate-v1"],
        }),
      );
      expect(received[1]).toBe(encodeJson({ hello: true }));
      expect(delivered).toEqual([encodeJson({ echoed: received[1]?.length })]);
    }),
  );

  it.live("maps a 4403 close to host-identity-mismatch", () =>
    Effect.gen(function* () {
      const script = (
        _frame: Uint8Array,
        _emit: (frame: Uint8Array) => void,
        close: (code?: number) => void,
      ): void => close(4403);
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(derivePublicKey(crypto.getRandomValues(new Uint8Array(32)))).toString(
          "base64url",
        ),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("host-identity-mismatch");
    }),
  );

  it.live("fails a write latched behind a rejected handshake", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const script = (
          _frame: Uint8Array,
          _emit: (frame: Uint8Array) => void,
          close: (code?: number) => void,
        ): void => close(4403);
        const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
          hostKey: Buffer.from(
            derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
          ).toString("base64url"),
          auth: { kind: "pairing", token: "t" },
        });
        const { write } = yield* socket.writer;
        const writeFiber = yield* Effect.forkChild(write("queued").pipe(Effect.exit), {
          startImmediately: true,
        });
        const runExit = yield* runString(socket, () => {}).pipe(Effect.exit);
        const writeExit = yield* Fiber.join(writeFiber);
        expect(findE2eeCause(runExit)?.reason).toBe("host-identity-mismatch");
        expect(findE2eeCause(writeExit)?.reason).toBe("host-identity-mismatch");
      }),
    ),
  );

  it.live("maps message-B AEAD failure to host-identity-mismatch", () =>
    Effect.gen(function* () {
      const pinnedKey = derivePublicKey(crypto.getRandomValues(new Uint8Array(32)));
      const script = (_frame: Uint8Array, emit: (frame: Uint8Array) => void): void => {
        const forged = crypto.getRandomValues(new Uint8Array(48));
        emit(forged);
      };
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(pinnedKey).toString("base64url"),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("host-identity-mismatch");
    }),
  );

  it.live("rejects a non-empty message-B payload as a protocol violation", () =>
    Effect.gen(function* () {
      const { hostKey, script } = responderScript({ messageBPayload: Uint8Array.of(1) });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("protocol");
    }),
  );

  it.live("rejects an empty encrypted continuation from the server", () =>
    Effect.gen(function* () {
      const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
      const hostKey = derivePublicKey(staticPrivate);
      const responder = createNkResponder({ staticPrivateKey: staticPrivate });
      let transport: NkTransport | null = null;
      const script = (frame: Uint8Array, emit: (frame: Uint8Array) => void): void => {
        if (transport === null) {
          responder.readMessageA(frame);
          emit(responder.writeMessageB(new Uint8Array(0)));
          transport = responder.split();
          return;
        }
        transport.receive.decryptWithAd(new Uint8Array(0), frame);
        emit(transport.send.encryptWithAd(new Uint8Array(0), Uint8Array.of(0x01)));
      };
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored-1" },
      });
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(exit._tag).toBe("Failure");
    }),
  );

  it.live("fails with unauthorized when the server rejects the credential", () =>
    Effect.gen(function* () {
      const { hostKey, script } = responderScript({ failAuth: true });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "expired" },
      });
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("unauthorized");
    }),
  );

  it.live("times out a stalled handshake", () =>
    Effect.gen(function* () {
      const socket = makeE2eeSocket(
        makeScriptedInnerSocket(() => {}),
        {
          hostKey: Buffer.from(
            derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
          ).toString("base64url"),
          auth: { kind: "bearer", credential: "t" },
          handshakeTimeoutMs: 100,
        },
      );
      const exit = yield* runString(socket, () => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("timeout");
    }),
  );

  it.live("fragments large writes and reassembles replies", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript();
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored-1" },
      });
      const large = encodeJson({ blob: "x".repeat(200_000) });
      const delivered: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.forkChild(
        Effect.scoped(
          Effect.gen(function* () {
            const { write } = yield* socket.writer;
            yield* Effect.forkChild(
              runString(
                socket,
                (text) => {
                  delivered.push(text);
                },
                { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
              ),
            );
            yield* Deferred.await(opened);
            yield* write(large);
            yield* Effect.sleep("100 millis");
          }),
        ),
      );
      yield* Effect.sleep("300 millis");
      yield* Fiber.interrupt(fiber);
      expect(received[1]).toBe(large);
      expect(delivered).toEqual([encodeJson({ echoed: large.length })]);
    }),
  );

  it.live("serializes concurrent logical messages while the first write is backpressured", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const { hostKey, script, received } = responderScript();
        const firstWriteStarted = yield* Deferred.make<void>();
        const releaseFirstWrite = yield* Deferred.make<void>();
        let blockNextWrite = false;
        const inner = makeScriptedInnerSocket(script, () => {
          if (!blockNextWrite) return Effect.void;
          blockNextWrite = false;
          return Deferred.succeed(firstWriteStarted, undefined).pipe(
            Effect.andThen(Deferred.await(releaseFirstWrite)),
          );
        });
        const socket = makeE2eeSocket(inner, {
          hostKey: Buffer.from(hostKey).toString("base64url"),
          auth: { kind: "bearer", credential: "stored-1" },
        });
        const opened = yield* Deferred.make<void>();
        const runFiber = yield* Effect.forkChild(
          runString(socket, () => {}, {
            onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid),
          }),
        );
        yield* Deferred.await(opened);

        const { write } = yield* socket.writer;
        encryptProbe.calls = 0;
        blockNextWrite = true;
        const firstMessage = encodeJson({ blob: "x".repeat(200_000) });
        const secondMessage = encodeJson({ ordinal: 2 });
        const writeFiber = yield* Effect.forkChild(write(firstMessage), {
          startImmediately: true,
        });
        yield* Deferred.await(firstWriteStarted);
        expect(encryptProbe.calls).toBe(1);
        const secondWriteFiber = yield* Effect.forkChild(write(secondMessage), {
          startImmediately: true,
        });
        yield* Effect.sleep("10 millis");
        expect(encryptProbe.calls).toBe(1);

        yield* Deferred.succeed(releaseFirstWrite, undefined);
        yield* Fiber.join(writeFiber);
        yield* Fiber.join(secondWriteFiber);
        expect(received.slice(1)).toEqual([firstMessage, secondMessage]);
        yield* Fiber.interrupt(runFiber);
      }),
    ),
  );
});
