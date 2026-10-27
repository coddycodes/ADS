import type { DeviceMediaTarget, MediaFrame } from "@coddy/client-runtime/device";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  createDecoderPipeline,
  createDeviceVideoWorkerRuntime,
  type DecoderPipelineOptions,
  type DeviceVideoWorkerEvent,
  FramePresenter,
  KEYFRAME_REQUEST_INTERVAL_MS,
  MAX_DECODE_QUEUE,
  workerPoster,
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
  decodeQueueSize = 0;
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

/**
 * A manual clock for the pipeline's `now` and `schedule`: time and timers move only in
 * `advance`, which runs each timer it reaches, in order.
 */
function fakeClock() {
  let time = 0;
  let nextId = 0;
  const timers = new Map<number, { readonly at: number; readonly callback: () => void }>();
  const now = () => time;
  const schedule = (callback: () => void, delayMs: number) => {
    const id = nextId++;
    timers.set(id, { at: time + delayMs, callback });
    return () => {
      timers.delete(id);
    };
  };
  return {
    now,
    options: { now, schedule } satisfies DecoderPipelineOptions,
    /** Timers scheduled and neither run nor cancelled. */
    get pending() {
      return timers.size;
    },
    advance(ms: number) {
      time += ms;
      const due = [...timers].filter(([, timer]) => timer.at <= time);
      for (const [id, timer] of due.toSorted(([, a], [, b]) => a.at - b.at)) {
        if (timers.delete(id)) timer.callback();
      }
    },
  };
}

function harness(options: DecoderPipelineOptions = {}) {
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
    unsupported: vi.fn(),
  };
  // Timers run only for a test that passes its own clock, so a held-back request leaks none.
  const pipeline = createDecoderPipeline(factory, events, {
    schedule: fakeClock().options.schedule,
    ...options,
  });
  return { decoders, events, pipeline, current: () => decoders.at(-1)! };
}

/** The decoder falls behind for one chunk: the pipeline drops it and waits for a key frame. */
function fallBehind(
  decoder: FakeDecoder,
  pipeline: { push(frame: MediaFrame): void },
  pts: number,
) {
  decoder.decodeQueueSize = MAX_DECODE_QUEUE + 1;
  pipeline.push(delta(pts));
  decoder.decodeQueueSize = 0;
}

/**
 * A backlog drop at 0 asks for a key frame; the recovery key frame decodes at 300; the decoder
 * falls behind again at 500, inside the rate-limit window, so that request is held back until 1000.
 */
function heldBackRequest() {
  const clock = fakeClock();
  const run = harness(clock.options);
  run.pipeline.push(config(SPS_PORTRAIT));
  run.pipeline.push(key(1));
  fallBehind(run.current(), run.pipeline, 2);
  clock.advance(300);
  run.pipeline.push(key(3));
  run.current().output(new FakeFrame(576, 1280));
  clock.advance(200);
  fallBehind(run.current(), run.pipeline, 4);
  return { clock, ...run };
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

  it("drops deltas and asks for a key frame when the decoder falls behind", () => {
    const { current, events, pipeline } = harness({ now: () => 0 });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    pipeline.push(delta(2));
    current().decodeQueueSize = MAX_DECODE_QUEUE + 1;
    pipeline.push(delta(3));
    current().decodeQueueSize = 0;
    // Caught up, but the GOP is broken: deltas wait for the next key frame.
    pipeline.push(delta(4));
    pipeline.push(key(5));
    pipeline.push(delta(6));
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 2, 5, 6]);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("never drops a key frame for backlog", () => {
    const { current, pipeline } = harness({ now: () => 0 });
    pipeline.push(config(SPS_PORTRAIT));
    current().decodeQueueSize = MAX_DECODE_QUEUE + 5;
    pipeline.push(key(1));
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1]);
  });

  it("asks for backlog key frames at most once a second", () => {
    let now = 0;
    const { current, events, pipeline } = harness({ now: () => now });
    pipeline.push(config(SPS_PORTRAIT));
    const behind = (pts: number) => {
      pipeline.push(key(pts));
      current().decodeQueueSize = MAX_DECODE_QUEUE + 1;
      pipeline.push(delta(pts + 1));
      current().decodeQueueSize = 0;
    };
    behind(1);
    now = KEYFRAME_REQUEST_INTERVAL_MS - 1;
    behind(3);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    now = KEYFRAME_REQUEST_INTERVAL_MS;
    behind(5);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
  });

  it("asks again for a throttled backlog key frame once the window has passed", () => {
    let now = 0;
    const { current, events, pipeline } = harness({ now: () => now });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    current().decodeQueueSize = MAX_DECODE_QUEUE + 1;
    pipeline.push(delta(2));
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    current().decodeQueueSize = 0;
    // A second backlog episode inside the window: its request is throttled.
    pipeline.push(key(3));
    now = 500;
    current().decodeQueueSize = MAX_DECODE_QUEUE + 1;
    pipeline.push(delta(4));
    current().decodeQueueSize = 0;
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    now = 900;
    pipeline.push(delta(5));
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    // The next dropped delta after the window asks again.
    now = KEYFRAME_REQUEST_INTERVAL_MS;
    pipeline.push(delta(6));
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 3]);
  });

  it("does not re-ask for key frames while waiting after a reconfigure", () => {
    let now = 0;
    const { events, pipeline } = harness({ now: () => now });
    pipeline.push(config(SPS_PORTRAIT));
    now = 5_000;
    pipeline.push(delta(1));
    pipeline.push(delta(2));
    expect(events.requestKeyframe).not.toHaveBeenCalled();
  });

  it("sends a held-back key frame request when its window ends, though no frame follows", () => {
    const { clock, events } = heldBackRequest();
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);

    // The screen is still, so the encoder sends nothing until it is asked.
    clock.advance(KEYFRAME_REQUEST_INTERVAL_MS - 500 - 1);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    clock.advance(1);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    clock.advance(10 * KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    expect(clock.pending).toBe(0);
  });

  it("starts a new window when the held-back request is sent", () => {
    const { clock, current, events, pipeline } = heldBackRequest();
    clock.advance(500);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    // A delta still dropped for the wait is held back until the second after that request.
    clock.advance(100);
    pipeline.push(delta(5));
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 3]);
    expect(clock.pending).toBe(1);
    clock.advance(KEYFRAME_REQUEST_INTERVAL_MS - 100 - 1);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    clock.advance(1);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(3);
  });

  it("keeps one held-back request pending however many are held back", () => {
    const { clock, current, events, pipeline } = heldBackRequest();
    clock.advance(100);
    fallBehind(current(), pipeline, 5);
    clock.advance(100);
    pipeline.push(delta(6));
    expect(clock.pending).toBe(1);
    clock.advance(KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
  });

  it("drops a held-back request when the key frame arrives before its window ends", () => {
    const { clock, current, events, pipeline } = heldBackRequest();
    expect(clock.pending).toBe(1);
    clock.advance(200);
    pipeline.push(key(7));
    expect(clock.pending).toBe(0);
    clock.advance(10 * KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 3, 7]);
  });

  it("drops a held-back request when the decoder is reconfigured", () => {
    const { clock, events, pipeline } = heldBackRequest();
    expect(clock.pending).toBe(1);
    // The reconfigure has its own key frame coming, so nothing more is asked for.
    pipeline.push(config(SPS_LANDSCAPE));
    expect(clock.pending).toBe(0);
    clock.advance(10 * KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("drops a held-back request when the pipeline stops", () => {
    const { clock, events, pipeline } = heldBackRequest();
    expect(clock.pending).toBe(1);
    pipeline.push({ kind: "config", ptsUs: 0n, data: new Uint8Array([0, 0, 0, 1, ...PPS]) });
    expect(events.failed).toHaveBeenCalledTimes(1);
    expect(clock.pending).toBe(0);
    clock.advance(10 * KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("drops a held-back request when it is disposed, leaving no timer", () => {
    const { clock, events, pipeline } = heldBackRequest();
    expect(clock.pending).toBe(1);
    pipeline.dispose();
    expect(clock.pending).toBe(0);
    clock.advance(10 * KEYFRAME_REQUEST_INTERVAL_MS);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("drops the held-back request when a request goes out directly after the window", () => {
    // A clock that passes the window without running the timer: the request is sent directly.
    let now = 0;
    const clock = fakeClock();
    const { current, events, pipeline } = harness({
      now: () => now,
      schedule: clock.options.schedule,
    });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    fallBehind(current(), pipeline, 2);
    pipeline.push(key(3));
    now = 500;
    fallBehind(current(), pipeline, 4);
    expect(clock.pending).toBe(1);
    now = KEYFRAME_REQUEST_INTERVAL_MS;
    pipeline.push(delta(5));
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
    expect(clock.pending).toBe(0);
  });

  it("holds back a request made when a codec check ends, until its window ends", async () => {
    const clock = fakeClock();
    const isCodecSupported = vi.fn(async () => true);
    const { events, pipeline } = harness({ ...clock.options, isCodecSupported });
    const settle = async () => {
      await Promise.resolve();
      await Promise.resolve();
    };
    // Frames beyond the hold limit are dropped, so the check ends with a key frame to ask for.
    const checkedStream = async (sps: ReadonlyArray<number>) => {
      pipeline.push(config(sps));
      for (let pts = 1; pts <= 31; pts++) pipeline.push(delta(pts));
      await settle();
    };
    await checkedStream(SPS_PORTRAIT);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);

    clock.advance(200);
    await checkedStream([0x67, 0x4d, 0x40, 0x28, 0x01]);
    expect(isCodecSupported).toHaveBeenCalledTimes(2);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(1);
    expect(clock.pending).toBe(1);
    clock.advance(KEYFRAME_REQUEST_INTERVAL_MS - 200);
    expect(events.requestKeyframe).toHaveBeenCalledTimes(2);
  });

  it("runs one codec check at a time and configures once", async () => {
    let answer: (supported: boolean) => void = () => undefined;
    const isCodecSupported = vi.fn(() => new Promise<boolean>((resolve) => (answer = resolve)));
    const { current, decoders, pipeline } = harness({ isCodecSupported });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(2));
    expect(isCodecSupported).toHaveBeenCalledTimes(1);
    answer(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(decoders).toHaveLength(1);
    expect(current().configured).toEqual(["avc1.42c01f"]);
    // Only the frames after the latest config are replayed.
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([2]);
  });

  it("checks the stream's codec once and replays the frames held meanwhile", async () => {
    const isCodecSupported = vi.fn(async () => true);
    const { current, decoders, events, pipeline } = harness({ isCodecSupported });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    pipeline.push(delta(2));
    expect(decoders).toHaveLength(0);
    await Promise.resolve();
    await Promise.resolve();
    expect(isCodecSupported).toHaveBeenCalledWith("avc1.42c01f");
    expect(current().decoded.map((chunk) => chunk.timestamp)).toEqual([1, 2]);
    pipeline.push(config(SPS_PORTRAIT));
    expect(isCodecSupported).toHaveBeenCalledTimes(1);
    expect(events.unsupported).not.toHaveBeenCalled();
  });

  it("reports an unsupported codec rather than a decoder error", async () => {
    const { decoders, events, pipeline } = harness({ isCodecSupported: async () => false });
    pipeline.push(config(SPS_PORTRAIT));
    pipeline.push(key(1));
    await Promise.resolve();
    await Promise.resolve();
    expect(events.unsupported).toHaveBeenCalledTimes(1);
    expect(events.failed).not.toHaveBeenCalled();
    expect(decoders).toHaveLength(0);
    pipeline.push(config(SPS_PORTRAIT));
    expect(events.unsupported).toHaveBeenCalledTimes(1);
  });

  it("gives up on a config frame without an SPS", () => {
    const { events, pipeline } = harness();
    pipeline.push({ kind: "config", ptsUs: 0n, data: new Uint8Array([0, 0, 0, 1, ...PPS]) });
    expect(events.failed).toHaveBeenCalledTimes(1);
  });
});

describe("createDeviceVideoWorkerRuntime", () => {
  const target: DeviceMediaTarget = { url: "ws://h/api/device-media", hostKey: null };

  function runtime(isCodecSupported?: (codec: string) => Promise<boolean>) {
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
      ...(isCodecSupported === undefined ? {} : { isCodecSupported }),
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

  it("reports a keepalive as liveness and never decodes it", () => {
    const { decoders, handle, posted, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    sockets[0]!.handlers.onFrame(key(1));
    const decoded = decoders[0]!.decoded.length;
    sockets[0]!.handlers.onFrame({ kind: "keepalive", ptsUs: 0n, data: new Uint8Array() });
    expect(decoders[0]!.decoded).toHaveLength(decoded);
    expect(posted.at(-1)).toEqual({ type: "alive" });
  });

  it("replaces the socket on a new start and ignores the old one", () => {
    const { handle, posted, sockets } = runtime();
    handle({ type: "start", target, ticket: "ticket-a" });
    sockets[0]!.handlers.onClose("expired");
    handle({ type: "start", target, ticket: "ticket-b" });
    handle({ type: "start", target, ticket: "ticket-c" });
    expect(sockets[1]!.close).toHaveBeenCalled();
    sockets[1]!.handlers.onClose("network");
    expect(posted).toEqual([{ type: "closed", reason: "expired" }]);
    expect(sockets.map((socket) => socket.ticket)).toEqual(["ticket-a", "ticket-b", "ticket-c"]);
  });

  it("asks the socket for a key frame when the decoder recovers", () => {
    const { decoders, handle, sockets } = runtime();
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    decoders[0]!.fail();
    expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(1);
  });

  it("sends a held-back key frame request through the socket when its window ends", () => {
    vi.useFakeTimers();
    try {
      const { decoders, handle, sockets } = runtime();
      handle({ type: "start", target, ticket: "T1" });
      const { onFrame } = sockets[0]!.handlers;
      onFrame(config(SPS_PORTRAIT));
      onFrame(key(1));
      fallBehind(decoders[0]!, { push: onFrame }, 2);
      expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(1);

      vi.advanceTimersByTime(300);
      onFrame(key(3));
      vi.advanceTimersByTime(200);
      fallBehind(decoders[0]!, { push: onFrame }, 4);
      expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(1);
      expect(vi.getTimerCount()).toBe(1);

      vi.advanceTimersByTime(KEYFRAME_REQUEST_INTERVAL_MS);
      expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(2);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves no timer running, or request to send, once stopped", () => {
    vi.useFakeTimers();
    try {
      const { decoders, handle, sockets } = runtime();
      handle({ type: "start", target, ticket: "T1" });
      const { onFrame } = sockets[0]!.handlers;
      onFrame(config(SPS_PORTRAIT));
      onFrame(key(1));
      fallBehind(decoders[0]!, { push: onFrame }, 2);
      onFrame(key(3));
      vi.advanceTimersByTime(200);
      fallBehind(decoders[0]!, { push: onFrame }, 4);
      expect(vi.getTimerCount()).toBe(1);

      handle({ type: "stop" });
      expect(vi.getTimerCount()).toBe(0);
      vi.advanceTimersByTime(10 * KEYFRAME_REQUEST_INTERVAL_MS);
      expect(sockets[0]!.requestKeyframe).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("posts unsupported when the stream's codec cannot be decoded", async () => {
    const { handle, posted, sockets } = runtime(async () => false);
    handle({ type: "start", target, ticket: "T1" });
    sockets[0]!.handlers.onFrame(config(SPS_PORTRAIT));
    await Promise.resolve();
    await Promise.resolve();
    expect(posted).toEqual([{ type: "unsupported" }]);
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

describe("FramePresenter", () => {
  it("draws only the newest frame per animation frame and closes the ones it replaces", () => {
    const callbacks: Array<() => void> = [];
    const drawn: FakeFrame[] = [];
    const presenter = new FramePresenter<FakeFrame>(
      (frame) => {
        drawn.push(frame);
        frame.close();
      },
      (callback) => callbacks.push(callback),
      () => undefined,
    );
    const [first, second, third] = [new FakeFrame(1, 1), new FakeFrame(1, 1), new FakeFrame(1, 1)];
    presenter.push(first);
    presenter.push(second);
    presenter.push(third);
    expect(callbacks).toHaveLength(1);
    expect(first.closed).toBe(true);
    expect(second.closed).toBe(true);
    expect(third.closed).toBe(false);
    callbacks[0]!();
    expect(drawn).toEqual([third]);
    expect(third.closed).toBe(true);
  });

  it("cancels its animation frame and closes the waiting frame and later arrivals once disposed", () => {
    const draw = vi.fn();
    const cancel = vi.fn();
    const presenter = new FramePresenter<FakeFrame>(draw, () => 42, cancel);
    const waiting = new FakeFrame(1, 1);
    presenter.push(waiting);
    presenter.dispose();
    expect(cancel).toHaveBeenCalledWith(42);
    expect(waiting.closed).toBe(true);
    const late = new FakeFrame(1, 1);
    presenter.push(late);
    expect(late.closed).toBe(true);
    expect(draw).not.toHaveBeenCalled();
  });
});

describe("workerPoster", () => {
  it("transfers frames", () => {
    const calls: Array<[unknown, unknown]> = [];
    const frame = new FakeFrame(1, 1);
    workerPoster<FakeFrame>((message, transfer) => calls.push([message, transfer]))({
      type: "frame",
      frame,
    });
    expect(calls).toEqual([[{ type: "frame", frame }, [frame]]]);
    expect(frame.closed).toBe(false);
  });

  it("clones a frame that cannot be transferred and closes its own copy", () => {
    const calls: Array<[unknown, unknown]> = [];
    const frame = new FakeFrame(1, 1);
    workerPoster<FakeFrame>((message, transfer) => {
      if (transfer !== undefined) throw new Error("DataCloneError");
      calls.push([message, transfer]);
    })({ type: "frame", frame });
    expect(calls).toEqual([[{ type: "frame", frame }, undefined]]);
    expect(frame.closed).toBe(true);
  });

  it("ends video when a frame can be neither transferred nor cloned", () => {
    const calls: unknown[] = [];
    const frame = new FakeFrame(1, 1);
    workerPoster<FakeFrame>((message) => {
      if (message.type === "frame") throw new Error("DataCloneError");
      calls.push(message);
    })({ type: "frame", frame });
    expect(calls).toEqual([{ type: "error" }]);
    expect(frame.closed).toBe(true);
  });
});
