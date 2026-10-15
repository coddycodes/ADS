import {
  avcCodecString,
  configSps,
  type DeviceMediaSocket,
  type DeviceMediaTarget,
  type MediaFrame,
} from "@bibcode/client-runtime/device";

/** Main thread → device video worker. */
export type DeviceVideoWorkerRequest =
  | { readonly type: "start"; readonly target: DeviceMediaTarget; readonly ticket: string }
  | { readonly type: "stop" };

/** Device video worker → main thread. */
export type DeviceVideoWorkerEvent<F = VideoFrame> =
  | { readonly type: "firstFrame" }
  /** Transferred; the receiver closes it. */
  | { readonly type: "frame"; readonly frame: F }
  | { readonly type: "resize"; readonly width: number; readonly height: number }
  /** The decoder failed twice in a row; video is over for this session. */
  | { readonly type: "error" }
  /** This client cannot decode the stream's codec; video is over for this session. */
  | { readonly type: "unsupported" }
  /** The media socket ended; `reason` is its close reason. */
  | { readonly type: "closed"; readonly reason: string }
  | { readonly type: "stopped" };

/** The parts of a `VideoFrame` the pipeline reads. */
export interface DecodedVideoFrame {
  readonly displayWidth: number;
  readonly displayHeight: number;
  close(): void;
}

export interface EncodedChunk {
  readonly type: "key" | "delta";
  readonly timestamp: number;
  readonly data: Uint8Array;
}

/** The parts of a `VideoDecoder` the pipeline drives; an errored decoder is `closed`. */
export interface VideoDecoderHandle {
  readonly state: "unconfigured" | "configured" | "closed";
  /** Chunks submitted and not yet decoded. */
  readonly decodeQueueSize: number;
  configure(config: { readonly codec: string; readonly optimizeForLatency: boolean }): void;
  decode(chunk: EncodedChunk): void;
  close(): void;
}

export type VideoDecoderFactory<F extends DecodedVideoFrame> = (init: {
  output(frame: F): void;
  error(error: unknown): void;
}) => VideoDecoderHandle;

export interface DecoderPipelineEvents<F extends DecodedVideoFrame> {
  /** Every decoded frame, in order; the receiver closes it. */
  frame(frame: F): void;
  /** The decoded picture's size changed (a rotation reconfigures the stream). */
  resize(width: number, height: number): void;
  firstFrame(): void;
  requestKeyframe(): void;
  /** The decoder failed again before decoding a frame after its one recovery. */
  failed(): void;
  /** This client cannot decode the stream's codec; video is over for this session. */
  unsupported(): void;
}

export interface DecoderPipelineOptions {
  /** Whether the decoder accepts `codec`, asked once per codec the stream announces. */
  readonly isCodecSupported?: (codec: string) => Promise<boolean>;
  readonly now?: () => number;
}

export interface DecoderPipeline {
  push(frame: MediaFrame): void;
  dispose(): void;
}

/** More chunks than this waiting in the decoder means it is falling behind the stream. */
export const MAX_DECODE_QUEUE = 3;
/** Backlog key frame requests are sent at most this often; the server limits resets too. */
export const KEYFRAME_REQUEST_INTERVAL_MS = 1_000;
/** Frames held while a codec check runs; more than this drops them and asks for a key frame. */
const MAX_FRAMES_DURING_CHECK = 30;

const concat = (head: Uint8Array, tail: Uint8Array): Uint8Array => {
  const bytes = new Uint8Array(head.length + tail.length);
  bytes.set(head);
  bytes.set(tail, head.length);
  return bytes;
};

/**
 * Feeds media frames to a decoder. Every config frame (re)configures it, after a one-time check
 * that its codec is supported, and deltas are dropped until the next key frame. The stream is
 * Annex-B with no `description`, so the config's SPS and PPS are prepended to every key frame.
 *
 * A decoder more than `MAX_DECODE_QUEUE` chunks behind drops deltas until the next key frame,
 * which it asks for at most once per `KEYFRAME_REQUEST_INTERVAL_MS`. A decoder error gets one
 * recovery (a new decoder, the cached config, and a key frame request); a second error before a
 * decoded frame gives up.
 */
export function createDecoderPipeline<F extends DecodedVideoFrame>(
  createDecoder: VideoDecoderFactory<F>,
  events: DecoderPipelineEvents<F>,
  options: DecoderPipelineOptions = {},
): DecoderPipeline {
  const now = options.now ?? (() => performance.now());
  const supportedCodecs = new Map<string, boolean>();
  let decoder: VideoDecoderHandle | null = null;
  let config: { readonly codec: string; readonly data: Uint8Array } | null = null;
  let waitingForKey = true;
  let recovering = false;
  let stopped = false;
  let sawFrame = false;
  let width = 0;
  let height = 0;
  let lastKeyframeRequest = Number.NEGATIVE_INFINITY;
  /** Frames that arrived while the codec check for the current config runs. */
  let held: MediaFrame[] | null = null;

  const closeDecoder = () => {
    const current = decoder;
    decoder = null;
    if (current !== null && current.state !== "closed") current.close();
  };

  const stop = (event: () => void) => {
    stopped = true;
    held = null;
    closeDecoder();
    event();
  };

  const requestKeyframeThrottled = () => {
    const at = now();
    if (at - lastKeyframeRequest < KEYFRAME_REQUEST_INTERVAL_MS) return;
    lastKeyframeRequest = at;
    events.requestKeyframe();
  };

  const onError = (source: VideoDecoderHandle) => {
    if (stopped || source !== decoder) return;
    closeDecoder();
    if (recovering) return stop(events.failed);
    recovering = true;
    configure();
    lastKeyframeRequest = now();
    events.requestKeyframe();
  };

  const open = (): VideoDecoderHandle => {
    const created: VideoDecoderHandle = createDecoder({
      output(frame) {
        if (stopped || created !== decoder) return frame.close();
        recovering = false;
        if (frame.displayWidth !== width || frame.displayHeight !== height) {
          width = frame.displayWidth;
          height = frame.displayHeight;
          events.resize(width, height);
        }
        if (!sawFrame) {
          sawFrame = true;
          events.firstFrame();
        }
        events.frame(frame);
      },
      error: () => onError(created),
    });
    return created;
  };

  const configure = () => {
    if (config === null) return;
    decoder ??= open();
    const current = decoder;
    waitingForKey = true;
    try {
      current.configure({ codec: config.codec, optimizeForLatency: true });
    } catch {
      onError(current);
    }
  };

  const decode = (chunk: EncodedChunk) => {
    const current = decoder;
    if (current === null || current.state !== "configured") return;
    if (chunk.type === "delta" && current.decodeQueueSize > MAX_DECODE_QUEUE) {
      // Behind: skip to the next key frame rather than queue more of this one's deltas.
      waitingForKey = true;
      requestKeyframeThrottled();
      return;
    }
    try {
      current.decode(chunk);
    } catch {
      onError(current);
    }
  };

  const feed = (frame: MediaFrame) => {
    if (config === null) return;
    if (frame.kind === "key") {
      waitingForKey = false;
      decode({
        type: "key",
        timestamp: Number(frame.ptsUs),
        data: concat(config.data, frame.data),
      });
      return;
    }
    if (waitingForKey) return;
    decode({ type: "delta", timestamp: Number(frame.ptsUs), data: frame.data });
  };

  const checkCodec = (codec: string) => {
    const check = options.isCodecSupported;
    if (check === undefined) return supportedCodecs.set(codec, true);
    held = [];
    void check(codec)
      .catch(() => false)
      .then((supported) => {
        if (stopped || config?.codec !== codec) return;
        supportedCodecs.set(codec, supported);
        if (!supported) return stop(events.unsupported);
        const waiting = held;
        held = null;
        configure();
        if (waiting === null) return requestKeyframeThrottled();
        for (const frame of waiting) feed(frame);
      });
  };

  return {
    push(frame) {
      if (stopped) return;
      if (frame.kind === "config") {
        let codec: string;
        try {
          codec = avcCodecString(configSps(frame.data));
        } catch {
          return stop(events.failed);
        }
        config = { codec, data: frame.data.slice() };
        held = null;
        const supported = supportedCodecs.get(codec);
        if (supported === false) return stop(events.unsupported);
        if (supported === undefined) {
          checkCodec(codec);
          if (supportedCodecs.get(codec) !== true) return;
        }
        configure();
        return;
      }
      if (held !== null) {
        // Too many while the check runs: drop them and start from a fresh key frame.
        if (held.length >= MAX_FRAMES_DURING_CHECK) held = null;
        else held.push(frame);
        return;
      }
      feed(frame);
    },
    dispose() {
      stopped = true;
      held = null;
      closeDecoder();
    },
  };
}

/**
 * Posts worker events, transferring each frame. Where frames cannot be transferred they are
 * cloned and this copy is closed; where they cannot be cloned either, video ends with `error`.
 */
export function workerPoster<F extends DecodedVideoFrame>(
  postMessage: (message: DeviceVideoWorkerEvent<F>, transfer?: Transferable[]) => void,
): (event: DeviceVideoWorkerEvent<F>) => void {
  return (event) => {
    if (event.type !== "frame") return postMessage(event);
    try {
      postMessage(event, [event.frame as unknown as Transferable]);
    } catch {
      try {
        postMessage(event);
      } catch {
        postMessage({ type: "error" });
      } finally {
        event.frame.close();
      }
    }
  };
}

/**
 * Main-thread drawing, latest first: at most one frame waits for the next animation frame, and a
 * newer arrival closes the one it replaces. A hidden page (no animation frames) holds one frame.
 */
export class FramePresenter<F extends { close(): void }> {
  private pending: F | null = null;
  private scheduled = false;
  private disposed = false;

  constructor(
    private readonly draw: (frame: F) => void,
    private readonly schedule: (callback: () => void) => void = (callback) =>
      void requestAnimationFrame(callback),
  ) {}

  push(frame: F): void {
    if (this.disposed) return frame.close();
    this.pending?.close();
    this.pending = frame;
    if (this.scheduled) return;
    this.scheduled = true;
    this.schedule(() => {
      this.scheduled = false;
      const next = this.pending;
      this.pending = null;
      if (next !== null && !this.disposed) this.draw(next);
      else next?.close();
    });
  }

  dispose(): void {
    this.disposed = true;
    this.pending?.close();
    this.pending = null;
  }
}

/** A WebCodecs `VideoDecoder` behind the pipeline's handle. */
export const webCodecsDecoder: VideoDecoderFactory<VideoFrame> = (init) => {
  const decoder = new VideoDecoder({ output: init.output, error: init.error });
  return {
    get state() {
      return decoder.state;
    },
    get decodeQueueSize() {
      return decoder.decodeQueueSize;
    },
    configure: (config) => decoder.configure(config),
    decode: (chunk) => decoder.decode(new EncodedVideoChunk(chunk)),
    close: () => decoder.close(),
  };
};

export interface DeviceVideoWorkerRuntimeDeps<F extends DecodedVideoFrame> {
  readonly post: (event: DeviceVideoWorkerEvent<F>) => void;
  readonly openSocket: (
    target: DeviceMediaTarget,
    ticket: string,
    handlers: { onFrame(frame: MediaFrame): void; onClose(reason: string): void },
  ) => DeviceMediaSocket;
  readonly createDecoder: VideoDecoderFactory<F>;
  readonly isCodecSupported?: (codec: string) => Promise<boolean>;
  /** Ends the worker after `stop`. */
  readonly exit: () => void;
}

/**
 * The worker's message handling: one decoder pipeline for the worker's life and one media socket
 * per `start`. A later `start` (a reconnect with a fresh ticket) replaces the socket; the server
 * sends its cached config first, so the decoder is reconfigured before the next key frame.
 */
export function createDeviceVideoWorkerRuntime<F extends DecodedVideoFrame>(
  deps: DeviceVideoWorkerRuntimeDeps<F>,
): (request: DeviceVideoWorkerRequest) => void {
  let socket: DeviceMediaSocket | null = null;
  let stopped = false;
  const pipeline = createDecoderPipeline(
    deps.createDecoder,
    {
      frame: (frame) => deps.post({ type: "frame", frame }),
      resize: (width, height) => deps.post({ type: "resize", width, height }),
      firstFrame: () => deps.post({ type: "firstFrame" }),
      requestKeyframe: () => socket?.requestKeyframe(),
      failed: () => deps.post({ type: "error" }),
      unsupported: () => deps.post({ type: "unsupported" }),
    },
    deps.isCodecSupported === undefined ? {} : { isCodecSupported: deps.isCodecSupported },
  );

  return (request) => {
    if (stopped) return;
    if (request.type === "stop") {
      stopped = true;
      socket?.close();
      socket = null;
      pipeline.dispose();
      deps.post({ type: "stopped" });
      deps.exit();
      return;
    }
    socket?.close();
    const current = deps.openSocket(request.target, request.ticket, {
      onFrame: (frame) => {
        if (socket === current) pipeline.push(frame);
      },
      onClose: (reason) => {
        if (socket !== current) return;
        socket = null;
        deps.post({ type: "closed", reason });
      },
    });
    socket = current;
  };
}
