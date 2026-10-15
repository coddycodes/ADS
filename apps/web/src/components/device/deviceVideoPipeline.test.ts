import type { DeviceMediaTarget, MediaFrame } from "@bibcode/client-runtime/device";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  createDecoderPipeline,
  createDeviceVideoWorkerRuntime,
  type DeviceVideoWorkerEvent,
  type EncodedChunk,
  type VideoDecoderFactory,
  type VideoDecoderHandle,
} from "./deviceVideoPipeline";

class FakeFrame {
  closed = false;
  constructor(
    readonly displayWidth: number,
    readonly displayHeight: number,
  ) {}
  close() {
    this.closed = true;
  }
}

class FakeDecoder implements VideoDecoderHandle {
  state: "unconfigured" | "configured" | "closed" = "unconfigured";
  readonly configured: string[] = [];
  readonly decoded: EncodedChunk[] = [];
  constructor(
    readonly output: (frame: FakeFrame) => void,
    readonly error: (error: unknown) => void,
  ) {}
  configure(config: { codec: string }) {
    this.state = "configured";
    this.configured.push(config.codec);
  }
  decode(chunk: EncodedChunk) {
    this.decoded.push(chunk);
  }
  close() {
    this.state = "closed";
  }
  /** As the browser does: an error closes the decoder, then reports. */
  fail() {
    this.state = "closed";
    this.error(new Error("decode failed"));
  }
}

const SPS_PORTRAIT = [0x67, 0x42, 0xc0, 0x1f, 0x01];
const SPS_LANDSCAPE = [0x67, 0x42, 0xc0, 0x1f, 0x02];
const PPS = [0x68, 0xce];

const config = (sps: ReadonlyArray<number>): MediaFrame => ({
  kind: "config",
  ptsUs: 0n,
  data: new Uint8Array([0, 0, 0, 1, ...sps, 0, 0, 0, 1, ...PPS]),
});
const key = (pts: number): MediaFrame => ({
  kind: "key",
  ptsUs: BigInt(pts),
  data: new Uint8Array([0, 0, 0, 1, 0x65, pts]),
});
const delta = (pts: number): MediaFrame => ({
  kind: "delta",
  ptsUs: BigInt(pts),
  data: new Uint8Array([0, 0, 0, 1, 0x41, pts]),
});

function harness() {
  const decoders: FakeDecoder[] = [];
  const factory: VideoDecoderFactory<FakeFrame> = (init) => {
    const decoder = new FakeDecoder(init.output, init.error);
    decoders.push(decoder);
    return decoder;
  };
  const events = {
    frame: vi.fn((frame: FakeFrame) => frame.close()),
    resize: vi.fn(),
    firstFrame: vi.fn(),
    requestKeyframe: vi.fn(),
    failed: vi.fn(),
  };
  const pipeline = createDecoderPipeline(factory, events);
  return { decoders, events, pipeline, current: () => decoders.at(-1)! };
}

describe("createDecoderPipeline", () => {
  it("configures from the config frame and prepends it to key frames", () => {
    const { current, events, pipeline } = harness();
    pipeline.push(delta(1));
    pipeline.push(config(SPS_PORTRAIT));
    expect(current().configured).toEqual(["avc1.42c01f"]);
    pipeline.push(key(2));
    pipeline.push(delta(3));
    expect(current().decoded.map((chunk) => [chunk.type, chunk.timestamp])).toEqual([
      ["key", 2],
      ["delta", 3],
    ]);
    expect([...current().decoded[0]!.data]).toEqual([...config(SPS_PORTRAIT).data, ...key(2).data]);

    current().output(new FakeFrame(576, 1280));
    current().output(new FakeFrame(576, 1280));
    expect(events.firstFrame).toHaveBeenCalledTimes(1);
    expect(events.resize.mock.calls).toEqual([[576, 1280]]);
    expect(events.frame).toHaveBeenCalledTimes(2);
  });

  it("reconfigures and resizes on a second config frame with swapped dimensions", () => {
    const { decoders, current, events, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    current().output(new FakeFrame(576, 1280));
    pipeline.push(config(SPS_LANDSCAPE));
    pipeline.push(key(2));
    current().output(new FakeFrame(1280, 576));

    expect(decoders).toHaveLength(1);
    expect(current().configured).toHaveLength(2);
    expect(events.resize.mock.calls).toEqual([
      [576, 1280],
      [1280, 576],
    ]);
    expect([...current().decoded[1]!.data]).toEqual([
      ...config(SPS_LANDSCAPE).data,
      ...key(2).data,
    ]);
  });

  it("drops deltas between a reconfigure and the next key frame", () => {
    const { current, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    pipeline.push(delta(2));
    pipeline.push(config(SPS_LANDSCAPE));
    pipeline.push(delta(3));
    pipeline.push(delta(4));
    pipeline.push(key(5));
    pipeline.push(delta(6));
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 2, 5, 6]);
  });

  it("recovers from one decoder error with a new decoder and a key frame request", () => {
    const { decoders, current, events, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    current().fail();

    expect(decoders).toHaveLength(2);
    expect(current().configured).toEqual(["avc1.42c01f"]);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    pipeline.push(delta(2));
    pipeline.push(key(3));
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([3]);
    expect(events.failed).not.toHaveBeenCalled();
  });

  it("gives up on a second error before any frame decodes", () => {
    const { current, events, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    current().fail();
    current().fail();
    expect(events.failed).toHaveBeenCalledTimes(1);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("forgives an earlier error once a frame decodes", () => {
    const { current, events, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    current().fail();
    pipeline.push(key(2));
    current().output(new FakeFrame(576, 1280));
    current().fail();
    expect(events.failed).not.toHaveBeenCalled();
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
  });

  it("closes frames that arrive after it is disposed", () => {
    const { current, events, pipeline } = harness();
    pipeline.push(config(SPS_PORTRAIT));
    const decoder = current();
    pipeline.dispose();
    expect(decoder.state).toBe("closed");
    const late = new FakeFrame(1, 1);
    decoder.output(late);
    expect(late.closed).toBe(true);
    expect(events.frame).not.toHaveBeenCalled();
  });

  it("gives up on a config frame without an SPS", () => {
    const { events, pipeline } = harness();
    pipeline.push({ kind: "config", ptsUs: 0n, data: new Uint8Array([0, 0, 0, 1, ...PPS]) });
    expect(events.failed).toHaveBeenCalledTimes(1);
  });
});

describe("createDeviceVideoWorkerRuntime", () => {
  const target: DeviceMediaTarget = { url: "ws://h/api/device-media", hostKey: null };

  function runtime() {
    const posted: DeviceVideoWorkerEvent<FakeFrame>[] = [];
    const sockets: Array<{
      ticket: string;
      handlers: { onFrame(frame: MediaFrame): void; onClose(reason: string): void };
      close: ReturnType<typeof vi.fn>;
      requestKeyframe: ReturnType<typeof vi.fn>;
    }> = [];
    const decoders: FakeDecoder[] = [];
    const exit = vi.fn();
    const handle = createDeviceVideoWorkerRuntime<FakeFrame>({
      post: (event) => posted.push(event),
      openSocket: (_target, ticket, handlers) => {
        const socket = { ticket, handlers, close: vi.fn(), requestKeyframe: vi.fn() };
        sockets.push(socket);
        return socket;
      },
      createDecoder: (init) => {
        const decoder = new FakeDecoder(init.output, init.error);
        decoders.push(decoder);
        return decoder;
      },
      exit,
    });
    return { decoders, exit, handle, posted, sockets };
  }

  it("streams one socket into the decoder and posts its events", () => {
    const { decoders, handle, posted, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    sockets[0]!.handlers.onFrame(key(1));
    const frame = new FakeFrame(576, 1280);
    decoders[0]!.output(frame);
    expect(posted).toEqual([
      { type: "resize", width: 576, height: 1280 },
      { type: "firstFrame" },
      { type: "frame", frame },
    ]);
  });

  it("replaces the socket on a new start and ignores the old one", () => {
    const { handle, posted, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onClose("expired");
    handle({ type: "start", target, ticket: "T2" });
    handle({ type: "start", target, ticket: "T3" });
    expect(sockets[1]!.close).toHaveBeenCalled();
    sockets[1]!.handlers.onClose("network");
    expect(posted).toEqual([{ type: "closed", reason: "expired" }]);
    expect(sockets.map((socket) => socket.ticket)).toEqual(["T1", "T2", "T3"]);
  });

  it("asks the socket for a key frame when the decoder recovers", () => {
    const { decoders, handle, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    decoders[0]!.fail();
    expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("closes the socket and the decoder on stop, then exits", () => {
    const { decoders, exit, handle, posted, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    handle({ type: "stop" });
    expect(sockets[0]!.close).toHaveBeenCalled();
    expect(decoders[0]!.state).toBe("closed");
    expect(posted.at(-1)).toEqual({ type: "stopped" });
    expect(exit).toHaveBeenCalled();
    handle({ type: "start", target, ticket: "T2" });
    expect(sockets).toHaveLength(1);
  });
});
