import type { DesktopPreviewTunnelBridge } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("../connection/runtime", () => ({ connectionAtomRuntime: {} }));
vi.mock("../state/session", () => ({ readPreparedConnection: () => null }));

import {
  createTunnelSlotsForTests,
  servePreviewTunnel,
  type PreviewTunnelTransport,
} from "./previewTunnelBroker";

const request = { connectionId: "c1", environmentId: "env", host: "localhost", port: 5173 };

/** A page connection whose sends `read` returns in order, then `null` once it closes. */
function fakeBridge(sent: Array<string | null>) {
  const written: string[] = [];
  let release: (() => void) | null = null;
  const bridge = {
    mode: async () => true,
    onRequest: () => () => undefined,
    answer: vi.fn(async () => undefined),
    read: vi.fn(async () => {
      if (sent.length > 0) return sent.shift()!;
      // Waits until the connection is closed.
      await new Promise<void>((resolve) => (release = resolve));
      return null;
    }),
    write: vi.fn(async (_id: string, data: string) => {
      written.push(data);
    }),
    close: vi.fn(async () => release?.()),
  } satisfies DesktopPreviewTunnelBridge;
  return { bridge, written };
}

describe("preview tunnel broker", () => {
  it("opens the tunnel, answers the page, and moves bytes both ways in order", async () => {
    const { bridge, written } = fakeBridge(["cmVx"]);
    const upstream: string[] = [];
    let writing = false;
    const transport: PreviewTunnelTransport = {
      open: async (_environmentId, port, handlers) => {
        expect(port).toBe(5173);
        await handlers.onOpened("t1");
        for (const chunk of ["YQ==", "Yg=="]) {
          // The next chunk is pulled only after the page took this one.
          expect(writing).toBe(false);
          writing = true;
          await handlers.onData(chunk);
          writing = false;
        }
        // Let the page's request reach the upstream before the server closes.
        await vi.waitFor(() => expect(upstream).toEqual(["cmVx"]));
      },
      write: async (_environmentId, tunnelId, data) => {
        expect(tunnelId).toBe("t1");
        upstream.push(data);
      },
      close: vi.fn(async () => undefined),
    };

    await servePreviewTunnel(request, bridge, transport);

    expect(bridge.answer).toHaveBeenCalledWith("c1", "opened");
    expect(written).toEqual(["YQ==", "Yg=="]);
    expect(bridge.close).toHaveBeenCalledWith("c1");
    expect(transport.close).toHaveBeenCalledWith("env", "t1");
  });

  it("refuses the page's connection when the tunnel cannot open", async () => {
    const { bridge } = fakeBridge([]);
    const transport: PreviewTunnelTransport = {
      open: async () => {
        throw new Error("no-upstream");
      },
      write: vi.fn(),
      close: vi.fn(),
    };

    await servePreviewTunnel(request, bridge, transport);

    expect(bridge.answer).toHaveBeenCalledWith("c1", "refused");
    expect(bridge.read).not.toHaveBeenCalled();
    expect(bridge.close).toHaveBeenCalledWith("c1");
    expect(transport.close).not.toHaveBeenCalled();
  });

  it("closes the tunnel and stops the stream when the page closes first", async () => {
    const { bridge } = fakeBridge([null]);
    let stopped = false;
    const transport: PreviewTunnelTransport = {
      open: (_environmentId, _port, handlers, signal) =>
        new Promise<void>((_resolve, reject) => {
          void handlers.onOpened("t1");
          signal.addEventListener("abort", () => {
            stopped = true;
            reject(new Error("stopped"));
          });
        }),
      write: vi.fn(),
      close: vi.fn(async () => undefined),
    };

    await servePreviewTunnel(request, bridge, transport);

    expect(stopped).toBe(true);
    expect(transport.close).toHaveBeenCalledWith("env", "t1");
    expect(bridge.answer).toHaveBeenCalledWith("c1", "opened");
    expect(bridge.answer).toHaveBeenCalledTimes(1);
  });

  it("connects the environment's own endpoint on this computer without a tunnel", async () => {
    const { bridge } = fakeBridge([]);
    const transport: PreviewTunnelTransport = { open: vi.fn(), write: vi.fn(), close: vi.fn() };

    // Only the endpoint's exact address: `localhost:5173` would be a dev server there.
    await servePreviewTunnel(
      { ...request, host: "127.0.0.1" },
      bridge,
      transport,
      (environmentId, host, port) =>
        environmentId === "env" && host === "127.0.0.1" && port === 5173,
    );

    expect(bridge.answer).toHaveBeenCalledWith("c1", "direct");
    expect(transport.open).not.toHaveBeenCalled();
    expect(bridge.close).not.toHaveBeenCalled();
  });

  it("lets a further connection wait for a slot instead of starving the RPC connection", async () => {
    const slots = createTunnelSlotsForTests(2);
    const first = await slots.take();
    const second = await slots.take();
    let third: (() => void) | null = null;
    void slots.take().then((release) => (third = release));
    await Promise.resolve();
    expect(third).toBeNull();
    expect(slots.waiting()).toBe(1);
    first();
    await vi.waitFor(() => expect(third).not.toBeNull());
    second();
    third!();
  });
});
