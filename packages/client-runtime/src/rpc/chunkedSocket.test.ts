import * as NodeZlib from "node:zlib";

import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Socket from "effect/socket/Socket";

import {
  CHUNKED_RPC_SUBPROTOCOL,
  DEFLATE_RPC_SUBPROTOCOL,
  makeChunkedSocket,
  plainSocketProtocols,
} from "./chunkedSocket.ts";
import { makeRawSocket, runRaw } from "../e2ee/rawSocket.ts";

const encoder = new TextEncoder();
const record = (flag: number, text: string) => {
  const bytes = encoder.encode(text);
  const out = new Uint8Array(1 + bytes.length);
  out[0] = flag;
  out.set(bytes, 1);
  return out;
};

/** A `0x10` record: the declared length, then raw deflate of `text`. */
const compressedRecord = (text: string, declared?: number) => {
  const body = encoder.encode(text);
  const deflated = NodeZlib.deflateRawSync(body);
  const out = new Uint8Array(1 + 4 + deflated.length);
  out[0] = 0x10;
  new DataView(out.buffer).setUint32(1, declared ?? body.length);
  out.set(deflated, 5);
  return out;
};

/** A socket that opens, delivers the given frames, then stays open. */
const scriptedSocket = (frames: ReadonlyArray<string | Uint8Array>): Socket.Socket =>
  makeRawSocket({
    runRaw: (handler, options) =>
      Effect.gen(function* () {
        if (options?.onOpen !== undefined) yield* options.onOpen;
        for (const frame of frames) {
          const result = handler(frame);
          if (Effect.isEffect(result)) yield* result;
        }
        return yield* Effect.never;
      }),
    writer: Effect.succeed({ write: () => Effect.void, writeAll: () => Effect.void }),
  });

const collect = (socket: Socket.Socket, count: number) =>
  Effect.gen(function* () {
    const received: Array<string | Uint8Array> = [];
    const fiber = yield* Effect.forkChild(
      runRaw(socket, (data) => {
        received.push(data);
      }),
    );
    // Inflation runs on the platform's DecompressionStream, outside the fiber scheduler:
    // tests that inflate run live and wait in real time, bounded.
    for (let attempt = 0; attempt < 400 && received.length < count; attempt += 1) {
      yield* Effect.yieldNow;
      if (attempt >= 100) {
        yield* Effect.sleep("5 millis");
      }
    }
    yield* Fiber.interrupt(fiber);
    return received;
  });

describe("makeChunkedSocket", () => {
  it.effect("reassembles records and passes control records through in order", () =>
    Effect.gen(function* () {
      const socket = makeChunkedSocket(
        scriptedSocket([
          "whole",
          record(0x01, "par"),
          record(0x02, "control"),
          record(0x00, "tial"),
        ]),
        () => CHUNKED_RPC_SUBPROTOCOL,
      );
      expect(yield* collect(socket, 3)).toEqual(["whole", "control", "partial"]);
    }),
  );

  it.effect("passes binary frames through unchanged without the subprotocol", () =>
    Effect.gen(function* () {
      const frame = record(0x00, "legacy");
      const socket = makeChunkedSocket(scriptedSocket([frame]), () => null);
      expect(yield* collect(socket, 1)).toEqual([frame]);
    }),
  );

  it.effect("fails the socket on a malformed record", () =>
    Effect.gen(function* () {
      const socket = makeChunkedSocket(
        scriptedSocket([Uint8Array.of(0x07, 0x20)]),
        () => CHUNKED_RPC_SUBPROTOCOL,
      );
      const error = yield* Effect.flip(runRaw(socket, () => undefined));
      expect(Socket.isSocketError(error)).toBe(true);
    }),
  );

  it.live("inflates compressed records once deflate is negotiated, in arrival order", () =>
    Effect.gen(function* () {
      const large = `{"data":"${"x".repeat(40_000)}"}`;
      const socket = makeChunkedSocket(
        scriptedSocket([compressedRecord(large), "after", record(0x00, "records")]),
        () => DEFLATE_RPC_SUBPROTOCOL,
      );
      expect(yield* collect(socket, 3)).toEqual([large, "after", "records"]);
    }),
  );

  it.effect("rejects compressed records the server was not allowed to send", () =>
    Effect.gen(function* () {
      const socket = makeChunkedSocket(
        scriptedSocket([compressedRecord("{}")]),
        () => CHUNKED_RPC_SUBPROTOCOL,
      );
      const error = yield* Effect.flip(runRaw(socket, () => undefined));
      expect(Socket.isSocketError(error)).toBe(true);
    }),
  );

  it.effect("fails the socket when a compressed message declares more than the cap", () =>
    Effect.gen(function* () {
      const socket = makeChunkedSocket(
        scriptedSocket([compressedRecord("{}", 65 * 1024 * 1024)]),
        () => DEFLATE_RPC_SUBPROTOCOL,
      );
      const error = yield* Effect.flip(runRaw(socket, () => undefined));
      expect(Socket.isSocketError(error)).toBe(true);
    }),
  );
});

describe("plainSocketProtocols", () => {
  it("offers compression to a remote server and an SSH tunnel, not to this machine", () => {
    const both = [DEFLATE_RPC_SUBPROTOCOL, CHUNKED_RPC_SUBPROTOCOL];
    expect(plainSocketProtocols({ socketUrl: "ws://10.0.0.5:3773/ws", tunnelled: false })).toEqual(
      both,
    );
    expect(plainSocketProtocols({ socketUrl: "ws://127.0.0.1:41000/ws", tunnelled: true })).toEqual(
      both,
    );
    expect(plainSocketProtocols({ socketUrl: "ws://127.0.0.1:3773/ws", tunnelled: false })).toEqual(
      [CHUNKED_RPC_SUBPROTOCOL],
    );
    expect(plainSocketProtocols({ socketUrl: "ws://localhost:3773/ws", tunnelled: false })).toEqual(
      [CHUNKED_RPC_SUBPROTOCOL],
    );
    expect(
      plainSocketProtocols({
        socketUrl: "ws://10.0.0.5:3773/ws",
        tunnelled: false,
        canInflate: false,
      }),
    ).toEqual([CHUNKED_RPC_SUBPROTOCOL]);
  });
});
