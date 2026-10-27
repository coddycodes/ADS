// @vitest-environment happy-dom
import { describe, expect, it } from "vite-plus/test";
import type { AdvertisedEndpoint } from "@coddy/contracts";

import { ShareTab } from "./ShareTab";
import { endpointDefaultPreferenceKey, selectPairingEndpoint } from "./shared";

function endpoint(input: {
  readonly id: string;
  readonly label: string;
  readonly host: string;
  readonly reachability: "loopback" | "lan" | "private-network" | "public";
  readonly isDefault?: boolean;
}): AdvertisedEndpoint {
  return {
    id: input.id,
    label: input.label,
    provider: { id: "desktop-core", label: "Desktop", kind: "core", isAddon: false },
    httpBaseUrl: `http://${input.host}:3773/`,
    wsBaseUrl: `ws://${input.host}:3773/`,
    reachability: input.reachability,
    compatibility: { hostedHttpsApp: "mixed-content-blocked", desktopApp: "compatible" },
    source: "desktop-core",
    status: "available",
    ...(input.isDefault === undefined ? {} : { isDefault: input.isDefault }),
  };
}

describe("ShareTab", () => {
  it("exports the moved share-side settings surface", () => {
    expect(typeof ShareTab).toBe("function");
  });

  it("selects an RFC1918 default but never auto-selects a public-only endpoint", () => {
    const lan = endpoint({
      id: "desktop-network:192.168.1.20:3773",
      label: "Local network",
      host: "192.168.1.20",
      reachability: "lan",
      isDefault: true,
    });
    const publicEndpoint = endpoint({
      id: "desktop-network:8.8.8.8:3773",
      label: "Public address",
      host: "8.8.8.8",
      reachability: "public",
      isDefault: false,
    });

    expect(selectPairingEndpoint([lan, publicEndpoint])).toBe(lan);
    expect(selectPairingEndpoint([publicEndpoint])).toBeNull();
    expect(
      selectPairingEndpoint([publicEndpoint], endpointDefaultPreferenceKey(publicEndpoint)),
    ).toBe(publicEndpoint);
  });

  it("ignores a public endpoint that incorrectly claims to be default", () => {
    const publicEndpoint = endpoint({
      id: "desktop-network:8.8.4.4:3773",
      label: "Public address",
      host: "8.8.4.4",
      reachability: "public",
      isDefault: true,
    });

    expect(selectPairingEndpoint([publicEndpoint])).toBeNull();
  });
});
