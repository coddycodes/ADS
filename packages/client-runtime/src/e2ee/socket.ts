import {
  E2eeAuthenticatedMessage,
  type E2eeAuthenticatedMessage as E2eeAuthenticatedMessageType,
} from "@bibcode/contracts";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";
import * as Scope from "effect/Scope";
import * as Socket from "effect/unstable/socket/Socket";

import { canInflateRawDeflate, makeOrderedDelivery } from "./deflate.ts";
import { MAX_E2EE_PREAUTH_MESSAGE_BYTES, plaintextRecords, RecordAssembler } from "./frame.ts";
import {
  createNkInitiator,
  decodeBase64UrlKey,
  NoiseAuthenticationError,
  type NkTransport,
} from "./noise.ts";

const decodeAuthenticated = Schema.decodeUnknownSync(E2eeAuthenticatedMessage);

export const E2EE_HANDSHAKE_TIMEOUT_MS = 10_000;
export const E2EE_HOST_IDENTITY_CLOSE_CODE = 4403;
/** Lets the server interleave `0x02` control records with a large message. */
export const E2EE_INTERLEAVE_FEATURE = "interleave-v1";
/** Lets the server send large messages raw-deflate compressed (`0x10` records). */
export const E2EE_DEFLATE_FEATURE = "deflate-v1";
const E2EE_CLIENT_FEATURES = canInflateRawDeflate()
  ? [E2EE_INTERLEAVE_FEATURE, E2EE_DEFLATE_FEATURE]
  : [E2EE_INTERLEAVE_FEATURE];

const EMPTY = new Uint8Array(0);
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const decodeJson = Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Unknown));
const encodeJson = Schema.encodeUnknownSync(Schema.fromJsonString(Schema.Unknown));

export type E2eeFailureReason = "host-identity-mismatch" | "unauthorized" | "protocol" | "timeout";

export class E2eeProtocolError extends Error {
  readonly reason: E2eeFailureReason;

  constructor(reason: E2eeFailureReason, detail: string) {
    super(detail);
    this.reason = reason;
  }
}

export type E2eeAuthRequest =
  | { readonly kind: "pairing"; readonly token: string }
  | { readonly kind: "bearer"; readonly credential: string };

export interface E2eeSocketOptions {
  readonly hostKey: string;
  readonly auth: E2eeAuthRequest;
  readonly handshakeTimeoutMs?: number;
  readonly onAuthenticated?: (message: E2eeAuthenticatedMessageType) => void;
}

const socketFailure = (error: E2eeProtocolError): Socket.SocketError =>
  new Socket.SocketError({ reason: new Socket.SocketReadError({ cause: error }) });

export const e2eeFailureOf = (error: unknown): E2eeProtocolError | null => {
  if (error instanceof E2eeProtocolError) return error;
  if (!Socket.isSocketError(error)) return null;
  const cause = "cause" in error.reason ? error.reason.cause : undefined;
  return cause instanceof E2eeProtocolError ? cause : null;
};

export const makeE2eeSocket = (inner: Socket.Socket, options: E2eeSocketOptions): Socket.Socket => {
  const responderKey = decodeBase64UrlKey(options.hostKey);
  const timeoutMs = options.handshakeTimeoutMs ?? E2EE_HANDSHAKE_TIMEOUT_MS;
  let transportDeferred = Deferred.makeUnsafe<NkTransport, Socket.SocketError>();
  let hasStarted = false;
  const outboundMessages = Semaphore.makeUnsafe(1);

  const encryptAndSend = (
    transport: NkTransport,
    write: (chunk: Uint8Array | string) => Effect.Effect<void, Socket.SocketError>,
    plaintext: Uint8Array,
  ): Effect.Effect<void, Socket.SocketError> =>
    Effect.gen(function* () {
      const records = yield* Effect.try({
        try: () => plaintextRecords(plaintext),
        catch: (cause) =>
          socketFailure(new E2eeProtocolError("protocol", `encrypt failed: ${String(cause)}`)),
      });
      for (const record of records) {
        const frame = yield* Effect.try({
          try: () => transport.send.encryptWithAd(EMPTY, record),
          catch: (cause) =>
            socketFailure(new E2eeProtocolError("protocol", `encrypt failed: ${String(cause)}`)),
        });
        yield* write(frame);
      }
    });

  const runRaw = <A, E, R>(
    handler: (data: string | Uint8Array) => Effect.Effect<A, E, R> | void,
    runOptions?: { readonly onOpen?: Effect.Effect<void> | undefined },
  ): Effect.Effect<void, Socket.SocketError | E, R> =>
    Effect.suspend(() => {
      const sessionTransport = hasStarted
        ? Deferred.makeUnsafe<NkTransport, Socket.SocketError>()
        : transportDeferred;
      hasStarted = true;
      transportDeferred = sessionTransport;
      const initiator = createNkInitiator({ responderStaticPublicKey: responderKey });
      let assembler = new RecordAssembler(MAX_E2EE_PREAUTH_MESSAGE_BYTES);
      const authenticated = Deferred.makeUnsafe<void, Socket.SocketError>();
      let phase: "handshake" | "auth" | "open" = "handshake";
      let transport: NkTransport | null = null;

      const fail = (reason: E2eeFailureReason, detail: string) => {
        const error = socketFailure(new E2eeProtocolError(reason, detail));
        Deferred.doneUnsafe(sessionTransport, Effect.fail(error));
        Deferred.doneUnsafe(authenticated, Effect.fail(error));
        return Effect.fail(error);
      };

      // Messages reach the handler in arrival order while one inflates.
      const deliver = makeOrderedDelivery(handler, (detail) =>
        fail("protocol", `E2EE message rejected: ${detail}`),
      );

      const currentTransport = (): NkTransport => {
        if (transport === null) throw new Error("E2EE transport is not ready");
        return transport;
      };

      const settleWriters = (): void => {
        const error = socketFailure(
          new E2eeProtocolError("protocol", "the E2EE socket is no longer running"),
        );
        Deferred.doneUnsafe(sessionTransport, Effect.fail(error));
        Deferred.doneUnsafe(authenticated, Effect.fail(error));
        if (transportDeferred === sessionTransport) {
          const unavailable = Deferred.makeUnsafe<NkTransport, Socket.SocketError>();
          Deferred.doneUnsafe(unavailable, Effect.fail(error));
          transportDeferred = unavailable;
        }
      };

      return Effect.scopedWith((scope) =>
        Effect.gen(function* () {
          const innerWrite = yield* Scope.provide(inner.writer, scope);

          const innerHandler = (
            delivered: string | Uint8Array,
          ): void | Effect.Effect<A | void, Socket.SocketError | E, R> => {
            // A browser WebSocket switched to binaryType "arraybuffer" delivers
            // ArrayBuffer frames, and the raw socket path hands them over
            // unchanged; they are binary and must be read as bytes.
            const data: string | Uint8Array =
              (delivered as unknown) instanceof ArrayBuffer
                ? new Uint8Array(delivered as unknown as ArrayBuffer)
                : delivered;
            if (typeof data === "string") {
              return fail("protocol", "peer sent a plaintext text frame on the E2EE channel");
            }
            if (!(data instanceof Uint8Array)) {
              // A Blob here means the underlying WebSocket was not switched to
              // binaryType "arraybuffer": asynchronous Blob conversion would
              // reorder ciphertext against the counter-based Noise nonce and
              // present as an intermittent protocol error. Fail closed and
              // name the invariant instead.
              return fail(
                "protocol",
                'the E2EE socket delivered a non-binary frame; the underlying WebSocket must use binaryType "arraybuffer"',
              );
            }

            switch (phase) {
              case "handshake": {
                let messageBPayload: Uint8Array;
                try {
                  messageBPayload = initiator.readMessageB(data);
                } catch (cause) {
                  return fail(
                    cause instanceof NoiseAuthenticationError
                      ? "host-identity-mismatch"
                      : "protocol",
                    `Noise handshake failed: ${String(cause)}`,
                  );
                }
                if (messageBPayload.length !== 0) {
                  return fail("protocol", "message B carried a non-empty handshake payload");
                }
                transport = initiator.split();
                phase = "auth";
                // The server decides from the consumed grant whether delivery
                // must be confirmed; the reply's pairingConfirmationRequired
                // is the only signal, so this also interoperates with servers
                // that predate the confirmation flow.
                const authMessage =
                  options.auth.kind === "pairing"
                    ? {
                        type: "e2ee_auth",
                        pairing: options.auth.token,
                        features: E2EE_CLIENT_FEATURES,
                      }
                    : {
                        type: "e2ee_auth",
                        bearer: options.auth.credential,
                        features: E2EE_CLIENT_FEATURES,
                      };
                return encryptAndSend(
                  currentTransport(),
                  innerWrite,
                  encoder.encode(encodeJson(authMessage)),
                );
              }
              case "auth":
              case "open": {
                let message: Uint8Array | null;
                try {
                  const record = currentTransport().receive.decryptWithAd(EMPTY, data);
                  message = assembler.push(record);
                } catch (cause) {
                  return fail("protocol", `E2EE frame rejected: ${String(cause)}`);
                }
                if (message === null) return undefined;
                if (phase === "open") return deliver(message, assembler.lastMessageDeflated);

                let parsed: { type?: string; code?: string };
                try {
                  const decoded = decodeJson(decoder.decode(message));
                  if (typeof decoded !== "object" || decoded === null || Array.isArray(decoded)) {
                    return fail("protocol", "control message was not a JSON object");
                  }
                  parsed = decoded as typeof parsed;
                } catch (cause) {
                  return fail("protocol", `unparsable control message: ${String(cause)}`);
                }
                if (parsed.type === "e2ee_authenticated") {
                  let ready: E2eeAuthenticatedMessageType;
                  try {
                    ready = decodeAuthenticated(parsed);
                  } catch (cause) {
                    return fail("protocol", `malformed e2ee_authenticated: ${String(cause)}`);
                  }
                  if (options.auth.kind === "pairing" && ready.credential === undefined) {
                    return fail("protocol", "the pairing bootstrap reply carried no credential");
                  }
                  try {
                    options.onAuthenticated?.(ready);
                  } catch (cause) {
                    return fail("protocol", `authenticated callback failed: ${String(cause)}`);
                  }
                  phase = "open";
                  assembler = new RecordAssembler(undefined, {
                    allowControlRecords: ready.features?.includes(E2EE_INTERLEAVE_FEATURE) === true,
                    allowDeflate: ready.features?.includes(E2EE_DEFLATE_FEATURE) === true,
                  });
                  Deferred.doneUnsafe(sessionTransport, Effect.succeed(currentTransport()));
                  Deferred.doneUnsafe(authenticated, Effect.void);
                  return undefined;
                }
                if (parsed.type === "e2ee_error") {
                  return fail(
                    parsed.code === "unauthorized" ? "unauthorized" : "protocol",
                    `server rejected the E2EE session (${parsed.code ?? "unknown"})`,
                  );
                }
                return fail("protocol", `unexpected control message ${parsed.type ?? "?"}`);
              }
            }
          };

          const sendMessageA = Effect.suspend(() =>
            innerWrite(initiator.writeMessageA(EMPTY)),
          ).pipe(
            Effect.catch((error) =>
              Effect.sync(() => {
                Deferred.doneUnsafe(sessionTransport, Effect.fail(error));
                Deferred.doneUnsafe(authenticated, Effect.fail(error));
              }),
            ),
          );
          const runInner = inner.runRaw(innerHandler, { onOpen: sendMessageA }).pipe(
            Effect.mapError((error) => {
              if (
                phase === "open" ||
                !Socket.isSocketError(error) ||
                error.reason._tag !== "SocketCloseError" ||
                error.reason.code !== E2EE_HOST_IDENTITY_CLOSE_CODE
              ) {
                return error;
              }
              const mismatch = socketFailure(
                new E2eeProtocolError(
                  "host-identity-mismatch",
                  "the host closed the handshake with code 4403",
                ),
              );
              Deferred.doneUnsafe(sessionTransport, Effect.fail(mismatch));
              Deferred.doneUnsafe(authenticated, Effect.fail(mismatch));
              return mismatch;
            }),
            Effect.andThen(
              Effect.suspend(() =>
                phase === "open"
                  ? Effect.void
                  : fail("protocol", "the E2EE socket closed before authentication"),
              ),
            ),
          );
          const deadline = Deferred.await(authenticated).pipe(
            Effect.timeoutOrElse({
              duration: `${timeoutMs} millis`,
              orElse: () => fail("timeout", "E2EE handshake did not complete in time"),
            }),
            Effect.andThen(
              runOptions?.onOpen === undefined
                ? Effect.never
                : Effect.andThen(runOptions.onOpen, Effect.never),
            ),
          );
          return yield* Effect.raceFirst(runInner, deadline).pipe(
            Effect.ensuring(Effect.sync(settleWriters)),
          );
        }),
      );
    });

  return Socket.make({
    runRaw,
    writer: Effect.gen(function* () {
      const innerWrite = yield* inner.writer;
      return (chunk: Uint8Array | string | Socket.CloseEvent) =>
        Socket.isCloseEvent(chunk)
          ? innerWrite(chunk)
          : outboundMessages.withPermits(1)(
              Deferred.await(transportDeferred).pipe(
                Effect.flatMap((readyTransport) =>
                  encryptAndSend(
                    readyTransport,
                    innerWrite,
                    typeof chunk === "string" ? encoder.encode(chunk) : chunk,
                  ),
                ),
              ),
            );
    }),
  });
};
