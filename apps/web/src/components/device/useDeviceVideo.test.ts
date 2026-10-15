// @vitest-environment happy-dom

import { EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const hookDeps = vi.hoisted(() => ({
  socketUrl: "wss://h.example/ws",
  mintTicket: vi.fn(async (_input: unknown) => ({ _tag: "Success", value: { ticket: "H1" } })),
}));

vi.mock("../../state/device", () => ({ deviceEnvironment: { createMediaTicket: {} } }));
vi.mock("../../state/session", async () => {
  const { some } = await import("effect/Option");
  return {
    usePreparedConnection: () => some({ socketUrl: hookDeps.socketUrl, e2ee: null }),
  };
});
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: () => hookDeps.mintTicket,
}));

import type { DeviceVideoWorkerEvent, DeviceVideoWorkerRequest } from "./deviceVideoPipeline";
import {
  type DeviceVideoWorker,
  DeviceVideoSession,
  useDeviceVideo,
  FIRST_FRAME_TIMEOUT_MS,
  STALL_TIMEOUT_MS,
  type VideoMode,
  WORKER_STOP_GRACE_MS,
} from "./useDeviceVideo";

class FakeWorker implements DeviceVideoWorker {
  readonly received: DeviceVideoWorkerRequest[] = [];
  terminated = false;
  private readonly messageListeners: Array<(event: MessageEvent<DeviceVideoWorkerEvent>) => void> =
    [];
  postMessage(message: DeviceVideoWorkerRequest) {
    this.received.push(message);
  }
  terminate() {
    this.terminated = true;
  }
  addEventListener(type: "message" | "error", listener: (event: never) => void) {
    if (type === "message") {
      this.messageListeners.push(listener as (event: MessageEvent<DeviceVideoWorkerEvent>) => void);
    }
  }
  emit(event: DeviceVideoWorkerEvent) {
    for (const listener of this.messageListeners) {
      listener({ data: event } as MessageEvent<DeviceVideoWorkerEvent>);
    }
  }
  get tickets() {
    return this.received.flatMap((message) => (message.type === "start" ? [message.ticket] : []));
  }
}

const target = { url: "ws://h/api/device-media", hostKey: null };

function session(options: { supported?: boolean } = {}) {
  const modes: VideoMode[] = [];
  const workers: FakeWorker[] = [];
  const drawn: unknown[] = [];
  let tickets = 0;
  let now = 0;
  const mintTicket = vi.fn(async () => `T${++tickets}`);
  const video = new DeviceVideoSession({
    isSupported: async () => options.supported ?? true,
    mintTicket,
    target: () => target,
    createWorker: () => {
      const worker = new FakeWorker();
      workers.push(worker);
      return worker;
    },
    draw: (frame) => drawn.push(frame),
    onFrameSize: vi.fn(),
    onMode: (mode) => modes.push(mode),
    now: () => now,
  });
  return {
    advance: async (ms: number) => {
      now += ms;
      await vi.advanceTimersByTimeAsync(ms);
    },
    drawn,
    mintTicket,
    modes,
    video,
    worker: () => workers[0]!,
    workers,
  };
}

const flush = () => vi.advanceTimersByTimeAsync(0);

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("DeviceVideoSession", () => {
  it("uses jpeg when VideoDecoder is missing", async () => {
    const { mintTicket, modes, video, workers } = session({ supported: false });
    video.start();
    await flush();
    expect(modes).toEqual([{ kind: "jpeg", reason: "unsupported" }]);
    expect(mintTicket).not.toHaveBeenCalled();
    expect(workers).toHaveLength(0);
  });

  it("switches to h264 on the first frame and draws every frame", async () => {
    const { drawn, modes, video, worker } = session();
    video.start();
    await flush();
    expect(worker().received).toEqual([{ type: "start", target, ticket: "T1" }]);
    const frame = { close: vi.fn() };
    worker().emit({ type: "firstFrame" });
    worker().emit({ type: "frame", frame: frame as unknown as VideoFrame });
    expect(modes).toEqual([{ kind: "probing" }, { kind: "h264", stalled: false }]);
    expect(drawn).toEqual([frame]);
  });

  it("falls back to jpeg when no frame arrives within 5 s", async () => {
    const { advance, modes, video, worker } = session();
    video.start();
    await flush();
    await advance(FIRST_FRAME_TIMEOUT_MS - 1);
    expect(modes).toEqual([{ kind: "probing" }]);
    await advance(1);
    expect(modes).toEqual([{ kind: "probing" }, { kind: "jpeg", reason: "noFirstFrame" }]);
    expect(worker().received.at(-1)).toEqual({ type: "stop" });
  });

  it("falls back to jpeg on decoder error and closes the socket", async () => {
    const { advance, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    worker().emit({ type: "error" });
    expect(modes).toEqual([
      { kind: "probing" },
      { kind: "h264", stalled: false },
      { kind: "jpeg", reason: "decoderError" },
    ]);
    // The worker closes its socket and decoder on `stop`, then is terminated.
    expect(worker().received.at(-1)).toEqual({ type: "stop" });
    expect(worker().terminated).toBe(false);
    const late = { close: vi.fn() };
    worker().emit({ type: "frame", frame: late as unknown as VideoFrame });
    expect(late.close).toHaveBeenCalled();
    worker().emit({ type: "stopped" });
    expect(worker().terminated).toBe(true);
    await advance(WORKER_STOP_GRACE_MS);
  });

  it("terminates a worker that never confirms the stop", async () => {
    const { advance, video, worker } = session();
    video.start();
    await flush();
    video.dispose();
    await advance(WORKER_STOP_GRACE_MS - 1);
    expect(worker().terminated).toBe(false);
    await advance(1);
    expect(worker().terminated).toBe(true);
  });

  it("re-requests a ticket on reconnect", async () => {
    const { advance, mintTicket, modes, video, worker, workers } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    worker().emit({ type: "closed", reason: "network" });
    await advance(500);
    expect(mintTicket).toHaveBeenCalledTimes(2);
    expect(worker().tickets).toEqual(["T1", "T2"]);
    // The same worker keeps decoding; only the socket is new.
    expect(workers).toHaveLength(1);
    expect(modes).toEqual([{ kind: "probing" }, { kind: "h264", stalled: false }]);
  });

  it("backs off and falls back after three reconnects in a minute", async () => {
    const { advance, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    for (const delay of [500, 1_000, 2_000]) {
      worker().emit({ type: "closed", reason: "expired" });
      await advance(delay - 1);
      const sent = worker().tickets.length;
      await advance(1);
      expect(worker().tickets.length).toBe(sent + 1);
    }
    worker().emit({ type: "closed", reason: "network" });
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "rejected" });
  });

  it("allows reconnects again once the minute has passed", async () => {
    const { advance, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    for (let index = 0; index < 3; index += 1) {
      worker().emit({ type: "closed", reason: "slow" });
      await advance(2_000);
    }
    await advance(60_000);
    worker().emit({ type: "closed", reason: "slow" });
    expect(modes.some((mode) => mode.kind === "jpeg")).toBe(false);
  });

  it("falls back at once when a new ticket cannot help", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    for (const reason of [
      "host-identity",
      "disabled",
      "unauthorized",
      "wrongDevice",
      "wrongChannel",
      "limit",
    ]) {
      const { mintTicket, modes, video, worker } = session();
      video.start();
      await flush();
      worker().emit({ type: "closed", reason });
      await vi.advanceTimersByTimeAsync(5_000);
      expect(modes).toEqual([{ kind: "probing" }, { kind: "jpeg", reason: "rejected" }]);
      expect(mintTicket).toHaveBeenCalledTimes(1);
    }
    // Only the channel mismatch is a client bug worth a warning.
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it("falls back at once when the device already has as many video viewers as it allows", async () => {
    const { mintTicket, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    worker().emit({ type: "closed", reason: "limit" });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "rejected" });
    expect(mintTicket).toHaveBeenCalledTimes(1);
  });

  it("stops trying video, without still images, when the device stopped", async () => {
    const { mintTicket, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    worker().emit({ type: "closed", reason: "stopped" });
    await vi.advanceTimersByTimeAsync(FIRST_FRAME_TIMEOUT_MS + 60_000);
    expect(modes).toEqual([{ kind: "probing" }, { kind: "h264", stalled: false }]);
    expect(mintTicket).toHaveBeenCalledTimes(1);
    expect(worker().received.at(-1)).toEqual({ type: "stop" });
  });

  it("stops trying video before its first frame when the device stopped", async () => {
    const { mintTicket, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "closed", reason: "stopped" });
    await vi.advanceTimersByTimeAsync(FIRST_FRAME_TIMEOUT_MS + 60_000);
    expect(modes).toEqual([{ kind: "probing" }]);
    expect(mintTicket).toHaveBeenCalledTimes(1);
  });

  it("marks video stalled after 5 s without a frame and clears it on the next", async () => {
    const { advance, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "firstFrame" });
    await advance(STALL_TIMEOUT_MS - 1);
    worker().emit({ type: "frame", frame: { close: vi.fn() } as unknown as VideoFrame });
    await advance(STALL_TIMEOUT_MS - 1);
    expect(modes).toEqual([{ kind: "probing" }, { kind: "h264", stalled: false }]);
    await advance(1);
    expect(modes.at(-1)).toEqual({ kind: "h264", stalled: true });
    worker().emit({ type: "frame", frame: { close: vi.fn() } as unknown as VideoFrame });
    expect(modes.at(-1)).toEqual({ kind: "h264", stalled: false });
    expect(modes).toHaveLength(4);
  });

  it("falls back as unsupported, without retrying, when the stream's codec is not", async () => {
    const { advance, mintTicket, modes, video, worker } = session();
    video.start();
    await flush();
    worker().emit({ type: "unsupported" });
    await advance(10_000);
    expect(modes).toEqual([{ kind: "probing" }, { kind: "jpeg", reason: "unsupported" }]);
    expect(mintTicket).toHaveBeenCalledTimes(1);
  });

  it("retries when the server will not issue a ticket", async () => {
    const { advance, mintTicket, video, workers } = session();
    mintTicket.mockResolvedValueOnce(null as unknown as string);
    video.start();
    await flush();
    expect(workers).toHaveLength(0);
    await advance(500);
    expect(workers[0]!.tickets).toEqual(["T1"]);
  });

  it("shows still images at once when the server never issues tickets", async () => {
    const { mintTicket, modes, video, workers } = session();
    mintTicket.mockResolvedValueOnce({ unavailable: "noSignIn" } as unknown as string);
    video.start();
    await flush();
    expect(workers).toHaveLength(0);
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "noSignIn" });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mintTicket).toHaveBeenCalledTimes(1);
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "noSignIn" });
  });

  it("does nothing after it is disposed", async () => {
    const { mintTicket, modes, video } = session();
    video.start();
    video.dispose();
    await flush();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mintTicket).not.toHaveBeenCalled();
    expect(modes).toEqual([]);
  });
});

describe("useDeviceVideo", () => {
  const environmentId = EnvironmentId.make("environment-work");

  async function renderHook(canvas: HTMLCanvasElement | null) {
    const modes: VideoMode[] = [];
    function Probe() {
      modes.push(useDeviceVideo({ environmentId, deviceId: "d-1", canvas }));
      return null;
    }
    const root = createRoot(document.createElement("div"));
    await act(async () => root.render(createElement(Probe)));
    return { modes, unmount: () => act(async () => root.unmount()) };
  }

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    hookDeps.mintTicket.mockClear();
    hookDeps.socketUrl = "wss://h.example/ws";
  });

  it("uses jpeg when the connection's socket URL does not parse", async () => {
    hookDeps.socketUrl = "not a url";
    vi.stubGlobal("VideoDecoder", { isConfigSupported: async () => ({ supported: true }) });
    const { modes, unmount } = await renderHook(document.createElement("canvas"));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "unsupported" });
    expect(hookDeps.mintTicket).not.toHaveBeenCalled();
    await unmount();
  });

  it("uses jpeg without minting a ticket where VideoDecoder is missing", async () => {
    vi.stubGlobal("VideoDecoder", undefined);
    const { modes, unmount } = await renderHook(document.createElement("canvas"));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "unsupported" });
    expect(hookDeps.mintTicket).not.toHaveBeenCalled();
    await unmount();
  });

  it("mints a ticket for the shown device and starts the worker", async () => {
    const posted: unknown[] = [];
    vi.stubGlobal("VideoDecoder", { isConfigSupported: async () => ({ supported: true }) });
    vi.stubGlobal(
      "Worker",
      class {
        addEventListener() {}
        postMessage(message: unknown) {
          posted.push(message);
        }
        terminate() {}
      },
    );
    const { modes, unmount } = await renderHook(document.createElement("canvas"));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(modes.at(-1)).toEqual({ kind: "probing" });
    expect(hookDeps.mintTicket).toHaveBeenCalledWith({
      environmentId,
      input: { deviceId: "d-1" },
    });
    expect(posted).toEqual([
      {
        type: "start",
        target: { url: "wss://h.example/api/device-media", hostKey: null },
        ticket: "H1",
      },
    ]);
    await unmount();
    expect(posted.at(-1)).toEqual({ type: "stop" });
  });

  it("uses jpeg at once when the server runs without sign-in", async () => {
    vi.stubGlobal("VideoDecoder", { isConfigSupported: async () => ({ supported: true }) });
    vi.stubGlobal(
      "Worker",
      class {
        addEventListener() {}
        postMessage() {}
        terminate() {}
      },
    );
    hookDeps.mintTicket.mockResolvedValueOnce({
      _tag: "Failure",
      cause: Cause.fail({ _tag: "DeviceMediaUnavailableError", message: "no sign-in" }),
    } as never);
    const { modes, unmount } = await renderHook(document.createElement("canvas"));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(modes.at(-1)).toEqual({ kind: "jpeg", reason: "noSignIn" });
    expect(hookDeps.mintTicket).toHaveBeenCalledTimes(1);
    await unmount();
  });
});

describe("useDeviceVideo restarts", () => {
  const environmentId = EnvironmentId.make("environment-work");

  it("shows probing again when a restarted session has not drawn yet", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    hookDeps.mintTicket.mockClear();
    const canvas = document.createElement("canvas");
    const workers: Array<{ emit(event: DeviceVideoWorkerEvent): void; posted: unknown[] }> = [];
    vi.stubGlobal("VideoDecoder", { isConfigSupported: async () => ({ supported: true }) });
    vi.stubGlobal(
      "Worker",
      class {
        readonly posted: unknown[] = [];
        private listener: ((event: { data: DeviceVideoWorkerEvent }) => void) | null = null;
        constructor() {
          workers.push(this);
        }
        addEventListener(
          type: string,
          listener: (event: { data: DeviceVideoWorkerEvent }) => void,
        ) {
          if (type === "message") this.listener = listener;
        }
        emit(event: DeviceVideoWorkerEvent) {
          this.listener?.({ data: event });
        }
        postMessage(message: unknown) {
          this.posted.push(message);
        }
        terminate() {}
      },
    );
    const modes: VideoMode[] = [];
    function Probe({ restart }: { readonly restart: number }) {
      modes.push(
        useDeviceVideo({
          environmentId,
          deviceId: "d-1",
          canvas,
          restart,
        }),
      );
      return null;
    }
    const root = createRoot(document.createElement("div"));
    await act(async () => root.render(createElement(Probe, { restart: 0 })));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    await act(async () => workers[0]!.emit({ type: "firstFrame" }));
    expect(modes.at(-1)).toEqual({ kind: "h264", stalled: false });

    await act(async () => root.render(createElement(Probe, { restart: 1 })));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(modes.at(-1)).toEqual({ kind: "probing" });
    expect(workers).toHaveLength(2);
    expect(hookDeps.mintTicket).toHaveBeenCalledTimes(2);
    expect(workers[0]!.posted.at(-1)).toEqual({ type: "stop" });
    await act(async () => root.unmount());
  });
});
