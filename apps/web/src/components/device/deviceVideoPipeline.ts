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
}

export interface DecoderPipeline {
  push(frame: MediaFrame): void;
  dispose(): void;
}

const concat = (head: Uint8Array, tail: Uint8Array): Uint8Array => {
  const bytes = new Uint8Array(head.length + tail.length);
  bytes.set(head);
  bytes.set(tail, head.length);
  return bytes;
};

/**
 * Feeds media frames to a decoder. Every config frame (re)configures it, and deltas are dropped
 * until the next key frame. The stream is Annex-B with no `description`, so the config's SPS and
 * PPS are prepended to every key frame. A decoder error gets one recovery (a new decoder, the
 * cached config, and a key frame request); a second error before a decoded frame gives up.
 */
export function createDecoderPipeline<F extends DecodedVideoFrame>(
  createDecoder: VideoDecoderFactory<F>,
  events: DecoderPipelineEvents<F>,
): DecoderPipeline {
  let decoder: VideoDecoderHandle | null = null;
  let config: { readonly codec: string; readonly data: Uint8Array } | null = null;
  let waitingForKey = true;
  let recovering = false;
  let stopped = false;
  let sawFrame = false;
  let width = 0;
  let height = 0;

  const fail = () => {
    stopped = true;
    closeDecoder();
    events.failed();
  };

  const closeDecoder = () => {
    const current = decoder;
    decoder = null;
    if (current !== null && current.state !== "closed") current.close();
  };

  const onError = (source: VideoDecoderHandle) => {
    if (stopped || source !== decoder) return;
    closeDecoder();
    if (recovering) return fail();
    recovering = true;
    configure();
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
    try {
      current.decode(chunk);
    } catch {
      onError(current);
    }
  };

  return {
    push(frame) {
      if (stopped) return;
      if (frame.kind === "config") {
        let codec: string;
        try {
          codec = avcCodecString(configSps(frame.data));
        } catch {
          return fail();
        }
        config = { codec, data: frame.data.slice() };
        configure();
        return;
      }
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
    },
    dispose() {
      stopped = true;
      closeDecoder();
    },
  };
}

/** A WebCodecs `VideoDecoder` behind the pipeline's handle. */
export const webCodecsDecoder: VideoDecoderFactory<VideoFrame> = (init) => {
  const decoder = new VideoDecoder({ output: init.output, error: init.error });
  return {
    get state() {
      return decoder.state;
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
  const pipeline = createDecoderPipeline(deps.createDecoder, {
    frame: (frame) => deps.post({ type: "frame", frame }),
    resize: (width, height) => deps.post({ type: "resize", width, height }),
    firstFrame: () => deps.post({ type: "firstFrame" }),
    requestKeyframe: () => socket?.requestKeyframe(),
    failed: () => deps.post({ type: "error" }),
  });

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
