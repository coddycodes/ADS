import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";

import { E2eeFrameError, MAX_E2EE_LOGICAL_MESSAGE_BYTES } from "./frame.ts";

/**
 * Whether this engine can decompress raw deflate. Compression (`deflate-v1` on
 * E2EE, the chunked-deflate subprotocol on plain `/ws`) is offered only then;
 * some engines ship DecompressionStream with gzip and deflate only.
 */
export const canInflateRawDeflate = (): boolean => {
  try {
    return new DecompressionStream("deflate-raw") instanceof DecompressionStream;
  } catch {
    return false;
  }
};

const decoder = new TextDecoder();

/**
 * A compressed message: a 4-byte big-endian uncompressed length, then raw deflate.
 * The output must match the declared length, which the message cap bounds.
 */
export const inflateMessage = async (message: Uint8Array, signal: AbortSignal): Promise<string> => {
  if (message.length < 4) throw new E2eeFrameError("truncated compressed message");
  const declared = new DataView(message.buffer, message.byteOffset, 4).getUint32(0);
  if (declared > MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
    throw new E2eeFrameError("compressed message exceeds the 64 MiB message cap");
  }
  const output = new Uint8Array(declared);
  const reader = new Blob([message.slice(4)])
    .stream()
    .pipeThrough(new DecompressionStream("deflate-raw"))
    .getReader();
  // A closed socket stops the inflation instead of finishing work nobody reads.
  const stop = () => void reader.cancel();
  signal.addEventListener("abort", stop, { once: true });
  let offset = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (offset + value.length > declared) {
        await reader.cancel();
        throw new E2eeFrameError("compressed message exceeds its declared size");
      }
      output.set(value, offset);
      offset += value.length;
    }
  } finally {
    signal.removeEventListener("abort", stop);
  }
  if (signal.aborted) throw new E2eeFrameError("compressed message abandoned");
  if (offset !== declared) throw new E2eeFrameError("compressed message is truncated");
  return decoder.decode(output);
};

/**
 * Hands reassembled messages to `handler` in arrival order, inflating compressed
 * ones. While one is being inflated, later ones wait until its handler's effect
 * has started, then take their turn. The handlers' effects still run
 * concurrently, as for uncompressed messages, so a stream waiting on a full queue
 * does not hold up other replies. A message dropped before its handler ran (it
 * failed to inflate, or the socket closed first) stops delivery, so nothing
 * behind it can overtake it. `reject` turns an inflation failure into the
 * socket's failure.
 */
export const makeOrderedDelivery = <A, E, R, F>(
  handler: (data: string) => Effect.Effect<A, E, R> | void,
  reject: (detail: string) => Effect.Effect<never, F>,
) => {
  let queueTail: Deferred.Deferred<void> | null = null;
  let deliveryStopped = false;
  const text = (message: Uint8Array | string) =>
    typeof message === "string" ? message : decoder.decode(message);
  // A text frame is already a whole, uncompressed message.
  return (
    message: Uint8Array | string,
    deflated: boolean,
  ): Effect.Effect<A | void, E | F, R> | void => {
    if (!deflated && queueTail === null) return handler(text(message));
    const previous = queueTail;
    const done = Deferred.makeUnsafe<void>();
    queueTail = done;
    let handled = false;
    const release = () => {
      if (!handled) deliveryStopped = true;
      Deferred.doneUnsafe(done, Effect.void);
      if (queueTail === done) queueTail = null;
    };
    return Effect.gen(function* () {
      if (previous !== null) yield* Deferred.await(previous);
      if (deliveryStopped) return;
      const data = deflated
        ? yield* Effect.tryPromise({
            try: (signal) => inflateMessage(message as Uint8Array, signal),
            catch: (cause) => String(cause),
          }).pipe(Effect.catch((detail) => reject(detail)))
        : text(message);
      const result = handler(data);
      handled = true;
      if (!Effect.isEffect(result)) return release();
      // Start the handler's effect before the next message's, as the socket does
      // for uncompressed messages, without waiting for it to finish.
      const running = yield* Effect.forkChild(result, { startImmediately: true });
      release();
      yield* Fiber.join(running);
    }).pipe(Effect.ensuring(Effect.sync(release)));
  };
};
