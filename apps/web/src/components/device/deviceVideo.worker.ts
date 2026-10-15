/* oxlint-disable unicorn/require-post-message-target-origin -- Worker messages have no target origin. */
// The device video worker: owns the media socket (and its Noise decryption) and the
// `VideoDecoder`, and transfers each decoded `VideoFrame` to the main thread, which draws and
// closes it. The logic lives in `deviceVideoPipeline.ts`, where it is tested.
import { openDeviceMediaSocket } from "@bibcode/client-runtime/device";

import {
  createDeviceVideoWorkerRuntime,
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

const post = (event: DeviceVideoWorkerEvent) => {
  if (event.type !== "frame") return scope.postMessage(event);
  try {
    scope.postMessage(event, [event.frame]);
  } catch {
    // Where frames cannot be transferred they are cloned, and this copy is ours to close.
    try {
      scope.postMessage(event);
    } catch {
      scope.postMessage({ type: "error" });
    } finally {
      event.frame.close();
    }
  }
};

const handle = createDeviceVideoWorkerRuntime({
  post,
  openSocket: openDeviceMediaSocket,
  createDecoder: webCodecsDecoder,
  exit: () => scope.close(),
});

scope.addEventListener("message", (event) => handle(event.data));
