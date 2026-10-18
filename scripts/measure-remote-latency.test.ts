import * as NodeZlib from "node:zlib";

import { describe, expect, it } from "vite-plus/test";

import type { WebSocketFrame } from "./lib/cdp.ts";
import {
  criticalPath,
  decodeFrames,
  echoLags,
  percentile,
  runMeasureRemoteLatencyMain,
  traceRpcs,
} from "./measure-remote-latency.ts";

const text = (at: number, direction: "sent" | "received", message: unknown, socket = "s1") =>
  ({
    at,
    direction,
    socket,
    url: "ws://127.0.0.1/ws",
    opcode: 1,
    payload: JSON.stringify(message),
    bytes: JSON.stringify(message).length,
  }) satisfies WebSocketFrame;

const binary = (at: number, record: Buffer): WebSocketFrame => ({
  at,
  direction: "received",
  socket: "s1",
  url: "ws://127.0.0.1/ws",
  opcode: 2,
  payload: record.toString("base64"),
  bytes: record.length,
});

describe("decodeFrames", () => {
  it("reassembles plain records, including deflated ones", () => {
    const message = JSON.stringify({ _tag: "Exit", requestId: "1", exit: { _tag: "Success" } });
    const plain = Buffer.from(message);
    const deflated = NodeZlib.deflateRawSync(plain);
    const length = Buffer.alloc(4);
    length.writeUInt32BE(plain.length);
    const compressed = Buffer.concat([length, deflated]);
    const control = Buffer.from(JSON.stringify({ _tag: "Pong" }));
    const decoded = decodeFrames([
      binary(1, Buffer.concat([Buffer.of(0x01), plain.subarray(0, 10)])),
      // A control record interleaves without disturbing the partial message.
      binary(2, Buffer.concat([Buffer.of(0x02), control])),
      binary(3, Buffer.concat([Buffer.of(0x00), plain.subarray(10)])),
      binary(4, Buffer.concat([Buffer.of(0x11), compressed.subarray(0, 5)])),
      binary(5, Buffer.concat([Buffer.of(0x10), compressed.subarray(5)])),
    ]);
    expect(decoded.map((entry) => [entry.at, entry.message._tag])).toEqual([
      [2, "Pong"],
      [3, "Exit"],
      [5, "Exit"],
    ]);
  });
});

describe("traceRpcs and criticalPath", () => {
  const frames = [
    text(0, "sent", { _tag: "Request", id: "1", tag: "a" }),
    text(0, "sent", { _tag: "Request", id: "2", tag: "parallel" }),
    text(250, "received", { _tag: "Exit", requestId: "1" }),
    text(252, "sent", { _tag: "Request", id: "3", tag: "b" }),
    text(260, "received", { _tag: "Exit", requestId: "2" }),
    text(500, "received", { _tag: "Exit", requestId: "3" }),
    // The same id on another socket is another request.
    text(501, "sent", { _tag: "Request", id: "1", tag: "c" }, "s2"),
    text(750, "received", { _tag: "Exit", requestId: "1" }, "s2"),
  ];

  it("pairs replies with requests per socket", () => {
    const traces = traceRpcs(decodeFrames(frames));
    expect(traces.map((trace) => [trace.tag, trace.firstReplyAt])).toEqual([
      ["a", 250],
      ["parallel", 260],
      ["b", 500],
      ["c", 750],
    ]);
  });

  it("counts requests that waited on a reply as sequential", () => {
    expect(criticalPath(traceRpcs(decodeFrames(frames)), 50)).toEqual({
      depth: 3,
      chain: ["a", "b", "c"],
    });
  });
});

describe("echoLags", () => {
  it("skips keys without an observable echo", () => {
    expect(
      echoLags(
        [0, 100, 200],
        "a b",
        [
          { at: 50, text: "a" },
          { at: 250, text: " b" },
        ],
        (value) => value.replace(/\s+/g, ""),
      ),
    ).toEqual([50, 50]);
  });

  it("measures each key until its echo arrived", () => {
    const lags = echoLags(
      [0, 100, 200],
      "abc",
      [
        { at: 120, text: "a" },
        { at: 400, text: "b\r\nc" },
      ],
      (value) => value.replace(/\s+/g, ""),
    );
    expect(lags).toEqual([120, 300, 200]);
    expect(percentile(lags, 50)).toBe(200);
    expect(percentile(lags, 95)).toBe(300);
  });
});

it("does not start the harness when imported", async () => {
  expect(await runMeasureRemoteLatencyMain(false, [])).toBe(false);
});
