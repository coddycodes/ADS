// The device media socket: H.264 units from `/api/device-media` (plain) or
// `/api/device-media-e2ee` (Noise NK with the pinned host key). No DOM; it runs in the
// device video worker.
import type { PreparedConnection } from "../connection/model.ts";
import { RecordAssembler, plaintextRecords } from "../e2ee/frame.ts";
import { createNkInitiator, decodeBase64UrlKey, type NkTransport } from "../e2ee/noise.ts";
import { MAX_DEVICE_MEDIA_FRAME_BYTES, type MediaFrame, parseMediaFrame } from "./mediaFrames.ts";

export const DEVICE_MEDIA_PATH = "/api/device-media";
export const DEVICE_MEDIA_E2EE_PATH = "/api/device-media-e2ee";
/** Separates media handshakes from `/ws-e2ee` ones; the server's responder uses the same bytes. */
export const DEVICE_MEDIA_NOISE_PROLOGUE = new TextEncoder().encode("coddy-studio-device-media/1");
/** The server's close code for a handshake against the wrong pinned host key. */
const HOST_IDENTITY_CLOSE_CODE = 4403;

/**
 * Close reasons besides the server's own (`expired`, `reused`, `wrongChannel`, `wrongDevice`,
 * `limit`, `disabled`, `stopped`, `failed`, `unauthorized`, `slow`): `host-identity` (wrong
 * pinned key or prologue),
 * `network` (refused before upgrade, as when the server's admission is full, or dropped without
 * a close frame), and `protocol` (a frame or record this client could not read).
 */
export type DeviceMediaLocalCloseReason = "host-identity" | "network" | "protocol";

export interface DeviceMediaTarget {
  readonly url: string;
  /** The pinned host key (base64url); set, the socket uses the E2EE route. */
  readonly hostKey: string | null;
}

export function deviceMediaTarget(
  prepared: Pick<PreparedConnection, "socketUrl" | "e2ee">,
): DeviceMediaTarget {
  const url = new URL(prepared.socketUrl);
  url.pathname = prepared.e2ee === null ? DEVICE_MEDIA_PATH : DEVICE_MEDIA_E2EE_PATH;
  // The media routes read only the ticket message; nothing in the RPC URL's query applies.
  url.search = "";
  url.hash = "";
  return { url: url.toString(), hostKey: prepared.e2ee?.hostKey ?? null };
}

/** The part of `WebSocket` the media socket uses. */
export interface MediaWebSocket {
  binaryType: string;
  readonly readyState: number;
  addEventListener(type: "open" | "error", listener: () => void): void;
  addEventListener(type: "message", listener: (event: { readonly data: unknown }) => void): void;
  addEventListener(
    type: "close",
    listener: (event: { readonly code: number; readonly reason: string }) => void,
  ): void;
  send(data: string | Uint8Array<ArrayBuffer>): void;
  close(): void;
}

export interface DeviceMediaHandlers {
  onFrame(frame: MediaFrame): void;
  /** Called once when the socket ends without `close()`; the reason decides what to do next. */
  onClose(reason: string): void;
}

export interface DeviceMediaSocket {
  requestKeyframe(): void;
  close(): void;
}

const OPEN = 1;
const EMPTY = new Uint8Array(0);
const encoder = new TextEncoder();

const toOwnedBytes = (bytes: Uint8Array): Uint8Array<ArrayBuffer> => {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy;
};

export function openDeviceMediaSocket(
  target: DeviceMediaTarget,
  ticket: string,
  handlers: DeviceMediaHandlers,
  deps: { readonly createSocket?: (url: string) => MediaWebSocket } = {},
): DeviceMediaSocket {
  let ended = false;
  const end = (reason: string) => {
    if (ended) return;
    ended = true;
    handlers.onClose(reason);
  };

  let responderKey: Uint8Array | null = null;
  if (target.hostKey !== null) {
    try {
      responderKey = decodeBase64UrlKey(target.hostKey);
    } catch {
      // Reported like any other close, never thrown at the caller.
      queueMicrotask(() => end("protocol"));
      return {
        requestKeyframe() {},
        close() {
          ended = true;
        },
      };
    }
  }

  const socket =
    deps.createSocket?.(target.url) ?? (new WebSocket(target.url) as unknown as MediaWebSocket);
  socket.binaryType = "arraybuffer";

  const fail = (reason: DeviceMediaLocalCloseReason) => {
    socket.close();
    end(reason);
  };

  const ticketMessage = JSON.stringify({ type: "ticket", ticket });
  const keyframeMessage = JSON.stringify({ type: "keyframe" });

  // E2EE: the handshake, then every message in and out is a record of the record layer.
  const initiator =
    responderKey === null
      ? null
      : createNkInitiator({
          responderStaticPublicKey: responderKey,
          prologue: DEVICE_MEDIA_NOISE_PROLOGUE,
        });
  let transport: NkTransport | null = null;
  const assembler = new RecordAssembler(MAX_DEVICE_MEDIA_FRAME_BYTES);

  const sendText = (text: string) => {
    if (socket.readyState !== OPEN) return;
    if (initiator === null) {
      socket.send(text);
      return;
    }
    if (transport === null) return;
    for (const record of plaintextRecords(encoder.encode(text))) {
      socket.send(toOwnedBytes(transport.send.encryptWithAd(EMPTY, record)));
    }
  };

  const deliver = (bytes: Uint8Array) => {
    handlers.onFrame(parseMediaFrame(bytes));
  };

  socket.addEventListener("open", () => {
    if (ended) return;
    if (initiator === null) socket.send(ticketMessage);
    else socket.send(toOwnedBytes(initiator.writeMessageA(EMPTY)));
  });
  socket.addEventListener("message", (event) => {
    if (ended) return;
    if (!(event.data instanceof ArrayBuffer)) return fail("protocol");
    const bytes = new Uint8Array(event.data);
    try {
      if (initiator === null) return deliver(bytes);
      if (transport === null) {
        initiator.readMessageB(bytes);
        transport = initiator.split();
        sendText(ticketMessage);
        return;
      }
      const message = assembler.push(transport.receive.decryptWithAd(EMPTY, bytes));
      if (message !== null) deliver(message);
    } catch {
      fail("protocol");
    }
  });
  socket.addEventListener("close", (event) => {
    if (event.code === HOST_IDENTITY_CLOSE_CODE) return end("host-identity");
    end(event.reason === "" ? "network" : event.reason);
  });

  return {
    requestKeyframe() {
      if (!ended) sendText(keyframeMessage);
    },
    close() {
      ended = true;
      socket.close();
    },
  };
}
