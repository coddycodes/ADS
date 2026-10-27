import type { ServerConfig } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const computeCompatVerdict = vi.hoisted(() => vi.fn());
vi.mock("@coddy/client-runtime/connection", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  computeCompatVerdict,
}));

import {
  resolveEnvironmentCompatVerdict,
  selectRemoteUpdateControlCapability,
} from "./environmentCompat";

function serverConfigWith(environment: Record<string, unknown>): ServerConfig {
  return { environment } as unknown as ServerConfig;
}

function serverConfigWithRemoteUpdateControl(remoteUpdateControl: boolean): ServerConfig {
  return serverConfigWith({ capabilities: { remoteUpdateControl } });
}

describe("resolveEnvironmentCompatVerdict", () => {
  it("returns null when the environment has never delivered a config", () => {
    expect(resolveEnvironmentCompatVerdict(null)).toBeNull();
    expect(computeCompatVerdict).not.toHaveBeenCalled();
  });

  it("delegates to the client-runtime verdict for a delivered descriptor", () => {
    const descriptor = { remoteProtocolVersion: 1, minCompatibleRemoteProtocol: 1 };
    computeCompatVerdict.mockReturnValue({ kind: "compatible" });
    expect(resolveEnvironmentCompatVerdict(serverConfigWith(descriptor))).toEqual({
      kind: "compatible",
    });
    expect(computeCompatVerdict).toHaveBeenCalledWith(descriptor);
  });
});

describe("selectRemoteUpdateControlCapability", () => {
  it("defaults to hidden for null config and for servers without the capability", () => {
    expect(selectRemoteUpdateControlCapability(null)).toBe(false);
    expect(selectRemoteUpdateControlCapability(serverConfigWithRemoteUpdateControl(false))).toBe(
      false,
    );
  });

  it("is true only for an explicit capability boolean", () => {
    expect(selectRemoteUpdateControlCapability(serverConfigWithRemoteUpdateControl(true))).toBe(
      true,
    );
  });
});
