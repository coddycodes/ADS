/* oxlint-disable unicorn/require-post-message-target-origin -- Worker messages have no target origin. */
// The device video worker: owns the media socket (and its Noise decryption) and the
// `VideoDecoder`, and transfers each decoded `VideoFrame` to the main thread, which draws and
// closes it. The logic lives in `deviceVideoPipeline.ts`, where it is tested.
import { openDeviceMediaSocket } from "@coddy/client-runtime/device";

import {
  createDeviceVideoWorkerRuntime,
  workerPoster,
  type DeviceVideoWorkerEvent,
  type DeviceVideoWorkerRequest,
  webCodecsDecoder,
} from "./deviceVideoPipeline";

/** The dedicated worker scope; the app's DOM lib types `self` as a window. */
interface WorkerScope {
  postMessage(message: DeviceVideoWorkerEvent, transfer?: Transferable[]): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<DeviceVideoWorkerRequest>) => void,
  ): void;
  close(): void;
}

const scope = self as unknown as WorkerScope;

const handle = createDeviceVideoWorkerRuntime({
  post: workerPoster((message, transfer) => scope.postMessage(message, transfer)),
  openSocket: openDeviceMediaSocket,
  createDecoder: webCodecsDecoder,
  isCodecSupported: async (codec) =>
    (await VideoDecoder.isConfigSupported({ codec, optimizeForLatency: true })).supported === true,
  exit: () => scope.close(),
});

scope.addEventListener("message", (event) => handle(event.data));
