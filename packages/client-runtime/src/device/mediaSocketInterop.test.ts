// Cross-language device media interop: the TypeScript media socket against the real Rust
// server. Opt-in: requires a freshly built server binary.
//
//   cargo build -p bibcode-server
//   BIBCODE_E2EE_SERVER_BIN=$PWD/target/debug/bibcode vp test run src/device/mediaSocketInterop.test.ts
//
// No device runs here (the real runtime has no fake SDK), so streaming is verified natively.
// What crosses the boundary is checked: the Noise handshake with the media prologue, the
// ticket inside the record layer, and the refusals. Device previews are on, since both media
// routes answer 503 before the upgrade while they are off. The server may then list the host's
// emulators and simulators, but no ticket here is real, so no device opens.
// @effect-diagnostics globalTimers:off - The harness bounds each wait.
import { afterAll, beforeAll, describe, expect, it } from "@effect/vitest";

import { createNkInitiator } from "../e2ee/noise.ts";
import {
  readHostPublicKey,
  type RunningServer,
  serverBinary,
  startServer,
  stopServer,
} from "../e2ee/testServer.ts";
import { DEVICE_MEDIA_E2EE_PATH, DEVICE_MEDIA_PATH, openDeviceMediaSocket } from "./mediaSocket.ts";

/** An unknown, well-formed ticket. */
const BOGUS_TICKET = "A".repeat(43);
/** The server waits 5 s for a ticket; a refusal well before that means the ticket arrived. */
const TICKET_WAIT_MS = 4_000;

const base64Url = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64url");

/** Opens the media socket and resolves with its close reason and how long it took. */
function closeReason(
  url: string,
  hostKey: string | null,
): Promise<{ reason: string; elapsedMs: number }> {
  const started = performance.now();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("media socket did not close")), 20_000);
    openDeviceMediaSocket({ url, hostKey }, BOGUS_TICKET, {
      onFrame: () => reject(new Error("no device streams in this test")),
      onClose: (reason) => {
        clearTimeout(timer);
        resolve({ reason, elapsedMs: performance.now() - started });
      },
    });
  });
}

describe.skipIf(serverBinary === undefined)("device media socket against the Rust server", () => {
  let server: RunningServer;
  let wsBase: string;
  let hostKey: string;

  beforeAll(async () => {
    server = await startServer({ settings: { enableDevicePreviews: true } });
    wsBase = server.httpBaseUrl.replace(/^http/, "ws");
    hostKey = base64Url(readHostPublicKey(server.dataRoot));
  }, 60_000);

  afterAll(async () => {
    if (server !== undefined) await stopServer(server);
  });

  it("delivers the ticket on the plain route", async () => {
    const { reason, elapsedMs } = await closeReason(`${wsBase}${DEVICE_MEDIA_PATH}`, null);
    expect(reason).toBe("expired");
    expect(elapsedMs).toBeLessThan(TICKET_WAIT_MS);
  }, 30_000);

  it("handshakes with the media prologue and delivers the ticket encrypted", async () => {
    const { reason, elapsedMs } = await closeReason(`${wsBase}${DEVICE_MEDIA_E2EE_PATH}`, hostKey);
    expect(reason).toBe("expired");
    expect(elapsedMs).toBeLessThan(TICKET_WAIT_MS);
  }, 30_000);

  it("closes with host-identity for the wrong pinned key", async () => {
    const wrongKey = base64Url(new Uint8Array(32).fill(9));
    const { reason } = await closeReason(`${wsBase}${DEVICE_MEDIA_E2EE_PATH}`, wrongKey);
    expect(reason).toBe("host-identity");
  }, 30_000);

  it("closes with 4403 for a handshake without the media prologue", async () => {
    const initiator = createNkInitiator({
      responderStaticPublicKey: readHostPublicKey(server.dataRoot),
    });
    const socket = new WebSocket(`${wsBase}${DEVICE_MEDIA_E2EE_PATH}`);
    socket.binaryType = "arraybuffer";
    const closed = new Promise<{ code: number; reason: string }>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("socket did not close")), 20_000);
      socket.addEventListener("close", (event) => {
        clearTimeout(timer);
        resolve({ code: event.code, reason: event.reason });
      });
    });
    socket.addEventListener("open", () =>
      socket.send(Uint8Array.from(initiator.writeMessageA(new Uint8Array()))),
    );
    expect(await closed).toEqual({ code: 4403, reason: "host-identity" });
  }, 30_000);
});
