/* oxlint-disable unicorn/require-post-message-target-origin -- Worker messages have no target origin. */
import { type DeviceMediaTarget, deviceMediaTarget } from "@bibcode/client-runtime/device";
import type { EnvironmentId } from "@bibcode/contracts";
import * as Option from "effect/Option";
import { useEffect, useRef, useState } from "react";

import { deviceEnvironment } from "../../state/device";
import { usePreparedConnection } from "../../state/session";
import { useAtomCommand } from "../../state/use-atom-command";
import type { StillImagesReason } from "./deviceMessages";
import {
  type DeviceVideoWorkerEvent,
  type DeviceVideoWorkerRequest,
  FramePresenter,
} from "./deviceVideoPipeline";

export type VideoMode =
  | { readonly kind: "probing" }
  /** `stalled`: no frame for `STALL_TIMEOUT_MS` since the last one. */
  | { readonly kind: "h264"; readonly stalled: boolean }
  | { readonly kind: "jpeg"; readonly reason: StillImagesReason };

/** The baseline codec probed before any ticket is minted; the stream's own is checked on use. */
export const PROBE_CODEC = "avc1.42E01F";
export const FIRST_FRAME_TIMEOUT_MS = 5_000;
/** scrcpy repeats the last frame of a still screen, so a silent stream this long has stalled. */
export const STALL_TIMEOUT_MS = 5_000;
export const MAX_RECONNECTS_PER_WINDOW = 3;
export const RECONNECT_WINDOW_MS = 60_000;
const RECONNECT_BASE_DELAY_MS = 500;
/** How long a stopping worker gets to close its socket and decoder before it is terminated. */
export const WORKER_STOP_GRACE_MS = 1_000;

/**
 * Close reasons a new ticket cannot fix, which switch to still images at once: the pinned host
 * key (or prologue) is wrong, previews were turned off, the session lost its rights, the device
 * is gone, the ticket was minted on another channel than the socket uses (a client bug that a
 * retry would only repeat), or the device already has as many video viewers as it allows.
 *
 * The server's other reasons: `stopped` (the device stopped or is not ready) ends the attempt
 * without still images, since the device list already shows the state and video starts again
 * with the next ready device; `expired`, `reused`, `failed`, and `slow` are retried, as are the
 * client's own `network` (a dropped or refused connection) and `protocol`.
 */
const FINAL_CLOSE_REASONS: ReadonlySet<string> = new Set([
  "host-identity",
  "disabled",
  "unauthorized",
  "wrongDevice",
  "wrongChannel",
  "limit",
]);

/** The parts of a `Worker` the session uses. */
export interface DeviceVideoWorker {
  postMessage(message: DeviceVideoWorkerRequest): void;
  terminate(): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<DeviceVideoWorkerEvent>) => void,
  ): void;
  addEventListener(type: "error", listener: () => void): void;
}

export interface DeviceVideoSessionDeps {
  /** Whether this client can decode the stream at all. */
  readonly isSupported: () => Promise<boolean>;
  /** A fresh single-use ticket, or `null` when the server would not issue one. */
  readonly mintTicket: () => Promise<string | null>;
  readonly target: () => DeviceMediaTarget | null;
  readonly createWorker: () => DeviceVideoWorker;
  /** Takes a decoded frame: draws it or closes it. */
  readonly draw: (frame: VideoFrame) => void;
  readonly onFrameSize: (width: number, height: number) => void;
  readonly onMode: (mode: VideoMode) => void;
  readonly now?: () => number;
}

/**
 * One device's H.264 attempt, from the support probe to video or the JPEG fallback. The worker
 * lives as long as the session; every (re)connect mints a new ticket. Falling back ends the
 * session: JPEG stays until the screen is opened again.
 */
export class DeviceVideoSession {
  private worker: DeviceVideoWorker | null = null;
  /** Terminates a stopping worker that never confirms the stop. */
  private killTimer: ReturnType<typeof setTimeout> | null = null;
  private firstFrameTimer: ReturnType<typeof setTimeout> | null = null;
  private stallTimer: ReturnType<typeof setTimeout> | null = null;
  private lastFrameAt = 0;
  private stalled = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly reconnects: number[] = [];
  private playing = false;
  private ended = false;

  constructor(private readonly deps: DeviceVideoSessionDeps) {}

  start(): void {
    this.deps.isSupported().then(
      (supported) => {
        if (this.ended) return;
        if (!supported) return this.fallBack("unsupported");
        // A restarted session shows "waiting" until its own first frame.
        this.deps.onMode({ kind: "probing" });
        this.firstFrameTimer = setTimeout(
          () => this.fallBack("noFirstFrame"),
          FIRST_FRAME_TIMEOUT_MS,
        );
        void this.connect();
      },
      () => this.fallBack("unsupported"),
    );
  }

  dispose(): void {
    this.end();
  }

  private async connect(): Promise<void> {
    const ticket = await this.deps.mintTicket().catch(() => null);
    if (this.ended) return;
    const target = this.deps.target();
    if (ticket === null || target === null) return this.closed("network");
    if (this.worker === null) {
      let worker: DeviceVideoWorker;
      try {
        worker = this.deps.createWorker();
      } catch {
        return this.fallBack("unsupported");
      }
      worker.addEventListener("message", (event) => this.receive(worker, event.data));
      worker.addEventListener("error", () => this.fallBack("decoderError"));
      this.worker = worker;
    }
    this.worker.postMessage({ type: "start", target, ticket });
  }

  private receive(worker: DeviceVideoWorker, event: DeviceVideoWorkerEvent): void {
    if (event.type === "frame") {
      if (this.ended) return event.frame.close();
      this.deps.draw(event.frame);
      this.lastFrameAt = this.now();
      if (this.stalled) {
        this.stalled = false;
        this.deps.onMode({ kind: "h264", stalled: false });
      }
      return;
    }
    if (this.ended) {
      if (event.type !== "stopped") return;
      if (this.killTimer !== null) clearTimeout(this.killTimer);
      this.killTimer = null;
      worker.terminate();
      return;
    }
    switch (event.type) {
      case "firstFrame":
        this.clearFirstFrameTimer();
        if (!this.playing) {
          this.playing = true;
          this.lastFrameAt = this.now();
          this.deps.onMode({ kind: "h264", stalled: false });
          this.watchForStall(STALL_TIMEOUT_MS);
        }
        return;
      case "resize":
        return this.deps.onFrameSize(event.width, event.height);
      case "error":
        return this.fallBack("decoderError");
      case "unsupported":
        return this.fallBack("unsupported");
      case "closed":
        return this.closed(event.reason);
      case "stopped":
        return;
    }
  }

  private closed(reason: string): void {
    if (reason === "wrongChannel") {
      console.warn("device video: media ticket was minted for another channel than its socket");
    }
    if (FINAL_CLOSE_REASONS.has(reason)) return this.fallBack("rejected");
    if (reason === "stopped") return this.end();
    const now = this.now();
    while (this.reconnects.length > 0 && now - this.reconnects[0]! >= RECONNECT_WINDOW_MS) {
      this.reconnects.shift();
    }
    if (this.reconnects.length >= MAX_RECONNECTS_PER_WINDOW) return this.fallBack("rejected");
    this.reconnects.push(now);
    const delay = RECONNECT_BASE_DELAY_MS * 2 ** (this.reconnects.length - 1);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect();
    }, delay);
  }

  private fallBack(reason: StillImagesReason): void {
    if (this.ended) return;
    this.end();
    this.deps.onMode({ kind: "jpeg", reason });
  }

  private now(): number {
    return this.deps.now?.() ?? Date.now();
  }

  /** One timer for the whole stream, re-armed for the time left rather than on every frame. */
  private watchForStall(delay: number): void {
    this.stallTimer = setTimeout(() => {
      this.stallTimer = null;
      if (this.ended) return;
      const silent = this.now() - this.lastFrameAt;
      if (silent < STALL_TIMEOUT_MS) return this.watchForStall(STALL_TIMEOUT_MS - silent);
      if (!this.stalled) {
        this.stalled = true;
        this.deps.onMode({ kind: "h264", stalled: true });
      }
      this.watchForStall(STALL_TIMEOUT_MS);
    }, delay);
  }

  private clearFirstFrameTimer(): void {
    if (this.firstFrameTimer !== null) clearTimeout(this.firstFrameTimer);
    this.firstFrameTimer = null;
  }

  private end(): void {
    if (this.ended) return;
    this.ended = true;
    this.clearFirstFrameTimer();
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    if (this.stallTimer !== null) clearTimeout(this.stallTimer);
    this.stallTimer = null;
    const worker = this.worker;
    this.worker = null;
    if (worker === null) return;
    // `terminate` drops queued messages, so the worker gets a moment to close its socket and
    // decoder, and confirms with `stopped`; frames still in flight are closed by `receive`.
    this.killTimer = setTimeout(() => worker.terminate(), WORKER_STOP_GRACE_MS);
    worker.postMessage({ type: "stop" });
  }
}

const hasWebCodecs = () => typeof VideoDecoder !== "undefined" && typeof Worker !== "undefined";

/** The media target for a prepared connection; `"invalid"` when its socket URL does not parse. */
function mediaTargetOf(
  prepared: Option.Option<Parameters<typeof deviceMediaTarget>[0]>,
): DeviceMediaTarget | "invalid" | null {
  if (Option.isNone(prepared)) return null;
  try {
    return deviceMediaTarget(prepared.value);
  } catch {
    return "invalid";
  }
}

const initialMode = (): VideoMode =>
  hasWebCodecs() ? { kind: "probing" } : { kind: "jpeg", reason: "unsupported" };

async function isH264Supported(): Promise<boolean> {
  if (!hasWebCodecs()) return false;
  const support = await VideoDecoder.isConfigSupported({ codec: PROBE_CODEC });
  return support.supported === true;
}

function createWorker(): DeviceVideoWorker {
  return new Worker(new URL("./deviceVideo.worker.ts", import.meta.url), {
    type: "module",
    name: "device-video",
  }) as DeviceVideoWorker;
}

/**
 * Draws onto `canvas` at the video's own size; CSS scales it into the letterbox. The backing
 * store changes only with the picture (a rotation), so a resized pane never clears it.
 */
function canvasPainter(canvas: HTMLCanvasElement): (frame: VideoFrame) => void {
  let context: CanvasRenderingContext2D | null = null;
  return (frame) => {
    try {
      if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
      if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
      context ??= canvas.getContext("2d");
      context?.drawImage(frame, 0, 0);
    } finally {
      frame.close();
    }
  };
}

/**
 * The device screen's video path: H.264 through a WebCodecs worker when this client can decode
 * it, else (or when it fails) JPEG. While the mode is not `jpeg` the hook draws on `canvas`;
 * `onFrameSize` reports the decoded picture's size, already oriented.
 */
export function useDeviceVideo(input: {
  readonly environmentId: EnvironmentId;
  readonly deviceId: string | null;
  readonly canvas: HTMLCanvasElement | null;
  readonly onFrameSize?: (width: number, height: number) => void;
  /** Changing it restarts video with a fresh ticket, as "Reconnect screen" does. */
  readonly restart?: number;
}): VideoMode {
  const { environmentId, deviceId, canvas, onFrameSize, restart = 0 } = input;
  const prepared = usePreparedConnection(environmentId);
  const resolved = mediaTargetOf(prepared);
  const target = resolved === "invalid" ? null : resolved;
  const targetUrl = target?.url ?? null;
  const hostKey = target?.hostKey ?? null;
  const mintTicket = useAtomCommand(deviceEnvironment.createMediaTicket, { reportFailure: false });

  // The mode belongs to one device and attempt; another device or a restart starts over.
  const sessionKey = `${environmentId}\u0000${deviceId ?? ""}\u0000${restart}`;
  const [state, setState] = useState(() => ({ key: sessionKey, mode: initialMode() }));
  const mode: VideoMode =
    resolved === "invalid"
      ? { kind: "jpeg", reason: "unsupported" }
      : state.key === sessionKey
        ? state.mode
        : initialMode();
  const fellBack = mode.kind === "jpeg";

  const onFrameSizeRef = useRef(onFrameSize);
  useEffect(() => {
    onFrameSizeRef.current = onFrameSize;
  });

  useEffect(() => {
    if (fellBack || deviceId === null || canvas === null || targetUrl === null) return;
    const presenter = new FramePresenter(canvasPainter(canvas));
    const session = new DeviceVideoSession({
      isSupported: isH264Supported,
      mintTicket: async () => {
        const result = await mintTicket({ environmentId, input: { deviceId } });
        return result._tag === "Success" ? result.value.ticket : null;
      },
      target: () => ({ url: targetUrl, hostKey }),
      createWorker,
      draw: (frame) => presenter.push(frame),
      onFrameSize: (width, height) => onFrameSizeRef.current?.(width, height),
      onMode: (next) => setState({ key: sessionKey, mode: next }),
    });
    session.start();
    return () => {
      session.dispose();
      presenter.dispose();
    };
  }, [canvas, deviceId, environmentId, fellBack, hostKey, mintTicket, sessionKey, targetUrl]);

  return mode;
}
