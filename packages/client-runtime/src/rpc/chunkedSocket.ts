import { isLoopbackHost } from "@bibcode/shared/preview";
import * as Effect from "effect/Effect";
import * as Socket from "effect/socket/Socket";

import { canInflateRawDeflate, makeOrderedDelivery } from "../e2ee/deflate.ts";
import { RecordAssembler } from "../e2ee/frame.ts";
import { makeRawSocket, runRaw } from "../e2ee/rawSocket.ts";

/** Subprotocol a plain `/ws` client offers to receive large messages as records. */
export const CHUNKED_RPC_SUBPROTOCOL = "bibcode.rpc.chunked.v1";
/**
 * {@link CHUNKED_RPC_SUBPROTOCOL} whose large server messages may also arrive
 * raw-deflate compressed, every record carrying `0x10` (as E2EE `deflate-v1`).
 */
export const DEFLATE_RPC_SUBPROTOCOL = "bibcode.rpc.chunked-deflate.v1";

/**
 * The subprotocols a plain socket offers, most preferred first. Compression is
 * offered when this engine can inflate raw deflate and the server is not on this
 * machine: a loopback socket is local unless it is an SSH tunnel's local end.
 */
export const plainSocketProtocols = (input: {
  readonly socketUrl: string;
  readonly tunnelled: boolean;
  readonly canInflate?: boolean;
}): Array<string> => {
  const local = !input.tunnelled && isLoopbackHost(new URL(input.socketUrl).hostname);
  return (input.canInflate ?? canInflateRawDeflate()) && !local
    ? [DEFLATE_RPC_SUBPROTOCOL, CHUNKED_RPC_SUBPROTOCOL]
    : [CHUNKED_RPC_SUBPROTOCOL];
};

const readFailure = (cause: unknown) =>
  new Socket.SocketError({ reason: new Socket.SocketReadError({ cause }) });

/**
 * Reassembles plain `/ws` records when the server selected
 * {@link CHUNKED_RPC_SUBPROTOCOL} or {@link DEFLATE_RPC_SUBPROTOCOL}. Text frames
 * are whole messages; binary frames are records (`0x00` final, `0x01`
 * continuation, `0x02` stand-alone control, and with deflate `0x10` on every
 * record of a compressed message). Without either subprotocol every frame passes
 * through unchanged, which is today's framing. `selected` is read at the first
 * frame, after the socket opened.
 */
export const makeChunkedSocket = (
  inner: Socket.Socket,
  selected: () => string | null | undefined,
): Socket.Socket =>
  makeRawSocket({
    runRaw: <A, E, R>(
      handler: (data: string | Uint8Array) => Effect.Effect<A, E, R> | void,
      options?: { readonly onOpen?: Effect.Effect<void> | undefined },
    ): Effect.Effect<void, Socket.SocketError | E, R> =>
      Effect.suspend(() => {
        let framing: { readonly records: boolean; readonly deflate: boolean } | null = null;
        let assembler: RecordAssembler | null = null;
        // Every message, text or record, goes through one ordered delivery, so a text
        // frame never overtakes a compressed message that is still inflating.
        const deliver = makeOrderedDelivery(handler, (detail) =>
          Effect.fail(readFailure(new Error(`compressed message rejected: ${detail}`))),
        );
        return runRaw<A | void, E | Socket.SocketError, R>(
          inner,
          (data) => {
            if (framing === null) {
              const protocol = selected();
              framing = {
                records:
                  protocol === CHUNKED_RPC_SUBPROTOCOL || protocol === DEFLATE_RPC_SUBPROTOCOL,
                deflate: protocol === DEFLATE_RPC_SUBPROTOCOL,
              };
              assembler = new RecordAssembler(undefined, {
                allowControlRecords: true,
                allowDeflate: framing.deflate,
              });
            }
            if (!framing.records) return handler(data);
            if (typeof data === "string") {
              return framing.deflate ? deliver(data, false) : handler(data);
            }
            const bytes =
              data instanceof Uint8Array ? data : new Uint8Array(data as unknown as ArrayBuffer);
            let message: Uint8Array | null;
            try {
              message = assembler!.push(bytes);
            } catch (cause) {
              return Effect.fail(readFailure(cause));
            }
            return message === null ? undefined : deliver(message, assembler!.lastMessageDeflated);
          },
          options,
        );
      }),
    writer: inner.writer,
  });
