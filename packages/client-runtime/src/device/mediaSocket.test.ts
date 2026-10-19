import { describe, expect, it } from "@effect/vitest";

import { splitIntoRecords } from "../e2ee/frame.ts";
import { createNkResponder, derivePublicKey, type NkTransport } from "../e2ee/noise.ts";
import {
  DEVICE_MEDIA_NOISE_PROLOGUE,
  deviceMediaTarget,
  openDeviceMediaSocket,
  type MediaWebSocket,
} from "./mediaSocket.ts";
import type { MediaFrame } from "./mediaFrames.ts";

const EMPTY = new Uint8Array(0);
const anyAuth = { kind: "bearer", credential: "c" } as const;

/** A WebSocket double: records what the client sends and lets a test play the server. */
class FakeSocket implements MediaWebSocket {
  static last: FakeSocket | null = null;
  binaryType = "blob";
  readyState = 0;
  readonly sent: Array<string | Uint8Array> = [];
  closedByClient = false;
  private readonly listeners = new Map<string, Array<(event: never) => void>>();

  readonly url: string;

  constructor(url: string) {
    this.url = url;
    FakeSocket.last = this;
  }

  addEventListener(type: string, listener: (event: never) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  send(data: string | Uint8Array): void {
    this.sent.push(typeof data === "string" ? data : data.slice());
  }

  close(): void {
    this.closedByClient = true;
    this.readyState = 3;
  }

  emit(type: "open" | "message" | "close" | "error", event: object = {}): void {
    if (type === "open") this.readyState = 1;
    for (const listener of this.listeners.get(type) ?? []) listener(event as never);
  }

  message(bytes: Uint8Array): void {
    const copy = bytes.slice();
    this.emit("message", { data: copy.buffer });
  }
}

const createSocket = (url: string) => new FakeSocket(url);

const frameBytes = (kind: number, pts: number, data: ReadonlyArray<number>) => {
  const bytes = new Uint8Array(9 + data.length);
  bytes[0] = kind;
  new DataView(bytes.buffer).setBigUint64(1, BigInt(pts));
  bytes.set(data, 9);
  return bytes;
};

const listen = () => {
  const frames: MediaFrame[] = [];
  const closes: string[] = [];
  return {
    frames,
    closes,
    handlers: {
      onFrame: (frame: MediaFrame) => frames.push(frame),
      onClose: (reason: string) => closes.push(reason),
    },
  };
};

const decoder = new TextDecoder();

describe("device media socket", () => {
  it("targets the plain or e2ee route on the same origin", () => {
    expect(deviceMediaTarget({ socketUrl: "wss://h.example/ws", e2ee: null }).url).toBe(
      "wss://h.example/api/device-media",
    );
    expect(
      deviceMediaTarget({
        socketUrl: "ws://10.0.0.2:3773/ws-e2ee",
        e2ee: { hostKey: "k", auth: anyAuth },
      }),
    ).toEqual({ url: "ws://10.0.0.2:3773/api/device-media-e2ee", hostKey: "k" });
  });

  it("never carries the RPC socket's query into the media URL", () => {
    expect(
      deviceMediaTarget({ socketUrl: "ws://127.0.0.1:3773/ws?wsToken=secret#x", e2ee: null }),
    ).toEqual({ url: "ws://127.0.0.1:3773/api/device-media", hostKey: null });
  });

  it("sends the ticket as the first message on the plain route", () => {
    const { frames, handlers } = listen();
    openDeviceMediaSocket({ url: "ws://h/api/device-media", hostKey: null }, "T1", handlers, {
      createSocket,
    });
    const socket = FakeSocket.last!;
    expect(socket.url).toBe("ws://h/api/device-media");
    expect(socket.binaryType).toBe("arraybuffer");
    expect(socket.sent).toEqual([]);
    socket.emit("open");
    expect(socket.sent).toEqual([JSON.stringify({ type: "ticket", ticket: "T1" })]);

    socket.message(frameBytes(0, 0, [0, 0, 0, 1, 0x67]));
    expect(frames).toEqual([
      { kind: "config", ptsUs: 0n, data: new Uint8Array([0, 0, 0, 1, 0x67]) },
    ]);
  });

  it("asks for a key frame in plain text", () => {
    const { handlers } = listen();
    const media = openDeviceMediaSocket(
      { url: "ws://h/api/device-media", hostKey: null },
      "T1",
      handlers,
      { createSocket },
    );
    const socket = FakeSocket.last!;
    socket.emit("open");
    media.requestKeyframe();
    expect(socket.sent[1]).toBe(JSON.stringify({ type: "keyframe" }));
  });

  it("reports the server's close reason once, and nothing after a local close", () => {
    const { closes, handlers } = listen();
    openDeviceMediaSocket({ url: "ws://h/api/device-media", hostKey: null }, "T1", handlers, {
      createSocket,
    });
    const socket = FakeSocket.last!;
    socket.emit("error");
    socket.emit("close", { code: 4400, reason: "expired" });
    expect(closes).toEqual(["expired"]);

    const second = listen();
    const media = openDeviceMediaSocket(
      { url: "ws://h/api/device-media", hostKey: null },
      "T2",
      second.handlers,
      { createSocket },
    );
    media.close();
    expect(FakeSocket.last!.closedByClient).toBe(true);
    FakeSocket.last!.emit("close", { code: 1005, reason: "" });
    expect(second.closes).toEqual([]);
  });

  it("names a refused or dropped connection, which carries no reason", () => {
    const { closes, handlers } = listen();
    openDeviceMediaSocket({ url: "ws://h/api/device-media", hostKey: null }, "T1", handlers, {
      createSocket,
    });
    FakeSocket.last!.emit("close", { code: 1006, reason: "" });
    expect(closes).toEqual(["network"]);
  });

  it("closes on a malformed frame", () => {
    const { closes, handlers } = listen();
    openDeviceMediaSocket({ url: "ws://h/api/device-media", hostKey: null }, "T1", handlers, {
      createSocket,
    });
    const socket = FakeSocket.last!;
    socket.emit("open");
    socket.message(new Uint8Array([9, 9]));
    expect(socket.closedByClient).toBe(true);
    expect(closes).toEqual(["protocol"]);
  });

  describe("e2ee route", () => {
    const hostPrivate = new Uint8Array(32).fill(7);
    const responder = () =>
      createNkResponder({ staticPrivateKey: hostPrivate, prologue: DEVICE_MEDIA_NOISE_PROLOGUE });
    // The pinned host key is the static public key, base64url without padding.
    const hostKey = Buffer.from(derivePublicKey(hostPrivate)).toString("base64url");

    const serve = () => {
      const { frames, closes, handlers } = listen();
      const media = openDeviceMediaSocket(
        { url: "ws://h/api/device-media-e2ee", hostKey },
        "T9",
        handlers,
        { createSocket },
      );
      const socket = FakeSocket.last!;
      socket.emit("open");
      const server = responder();
      const messageA = socket.sent[0] as Uint8Array;
      server.readMessageA(messageA);
      socket.message(server.writeMessageB(EMPTY));
      const transport = server.split();
      return { frames, closes, media, socket, transport };
    };

    const decryptSent = (transport: NkTransport, records: ReadonlyArray<string | Uint8Array>) =>
      records.map((record) => {
        const plaintext = transport.receive.decryptWithAd(EMPTY, record as Uint8Array);
        expect(plaintext[0]).toBe(0);
        return decoder.decode(plaintext.subarray(1));
      });

    it("handshakes with the media prologue and sends the ticket encrypted", () => {
      const { socket, transport } = serve();
      expect(decryptSent(transport, socket.sent.slice(1))).toEqual([
        JSON.stringify({ type: "ticket", ticket: "T9" }),
      ]);
    });

    it("reassembles frames that span several records", () => {
      const { frames, socket, transport } = serve();
      const payload = new Uint8Array(70_000).map((_, index) => index % 251);
      const frame = new Uint8Array(9 + payload.length);
      frame[0] = 1;
      frame.set(payload, 9);
      const records = splitIntoRecords(frame);
      expect(records.length).toBeGreaterThan(1);
      for (const record of records) socket.message(transport.send.encryptWithAd(EMPTY, record));
      expect(frames).toHaveLength(1);
      expect(frames[0]!.kind).toBe("key");
      expect(frames[0]!.data).toEqual(payload);
    });

    it("encrypts a key frame request", () => {
      const { media, socket, transport } = serve();
      media.requestKeyframe();
      expect(decryptSent(transport, socket.sent.slice(1))).toEqual([
        JSON.stringify({ type: "ticket", ticket: "T9" }),
        JSON.stringify({ type: "keyframe" }),
      ]);
    });

    it("fails the handshake against a responder without the media prologue", () => {
      const { closes, handlers } = listen();
      openDeviceMediaSocket({ url: "ws://h/api/device-media-e2ee", hostKey }, "T9", handlers, {
        createSocket,
      });
      const socket = FakeSocket.last!;
      socket.emit("open");
      const plainResponder = createNkResponder({ staticPrivateKey: hostPrivate });
      expect(() => plainResponder.readMessageA(socket.sent[0] as Uint8Array)).toThrow();
      expect(closes).toEqual([]);
    });

    it("closes when a record does not decrypt", () => {
      const { closes, socket } = serve();
      socket.message(new Uint8Array(40).fill(1));
      expect(socket.closedByClient).toBe(true);
      expect(closes).toEqual(["protocol"]);
    });

    it("reports a malformed host key as a protocol close", async () => {
      const { closes, handlers } = listen();
      openDeviceMediaSocket(
        { url: "ws://h/api/device-media-e2ee", hostKey: "not a key" },
        "T9",
        handlers,
        { createSocket },
      );
      await Promise.resolve();
      expect(closes).toEqual(["protocol"]);
    });
  });
});
