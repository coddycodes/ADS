import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";

import {
  managedEndpointDigestInput,
  managedEndpointForHostname,
  managedEndpointHostname,
  managedEndpointPreviewHostnames,
  isManagedEndpointHostname,
  managedEndpointTunnelName,
  relayOwnsManagedEndpointZone,
  RelayPublicDomainLabelTooLongError,
  relayPublicDomainForStage,
  relayResourceNameForStage,
  relayStageSlug,
} from "./deploymentConfig.ts";

const isRelayPublicDomainLabelTooLongError = Schema.is(RelayPublicDomainLabelTooLongError);

describe("relayStageSlug", () => {
  it("matches Alchemy physical-name sanitization for default developer stages", () => {
    expect(relayStageSlug("dev_julius")).toBe("dev-julius");
  });
});

describe("relayPublicDomainForStage", () => {
  it("uses the canonical relay hostname for production", () => {
    expect(relayPublicDomainForStage("prod", ".example.com.")).toBe("relay.example.com");
  });

  it("isolates personal stages below the imported zone", () => {
    expect(relayPublicDomainForStage("dev_julius", "example.com")).toBe(
      "relay-dev-julius.example.com",
    );
  });

  it("reports the stage and derived DNS label when the label is too long", () => {
    const stage = `dev_${"x".repeat(60)}`;
    let error: unknown;

    try {
      relayPublicDomainForStage(stage, "example.com");
    } catch (cause) {
      error = cause;
    }

    if (!isRelayPublicDomainLabelTooLongError(error)) {
      throw error;
    }
    expect(error).toMatchObject({
      stage,
      label: `relay-dev-${"x".repeat(60)}`,
      maxLength: 63,
    });
    expect(error.message).toBe(
      `Relay stage '${stage}' produces custom domain label 'relay-dev-${"x".repeat(60)}' (70 characters), exceeding the DNS label limit of 63.`,
    );
  });
});

describe("relayOwnsManagedEndpointZone", () => {
  it("keeps the shared Cloudflare zone owned by production", () => {
    expect(relayOwnsManagedEndpointZone("prod")).toBe(true);
    expect(relayOwnsManagedEndpointZone("dev_julius")).toBe(false);
  });
});

describe("relayResourceNameForStage", () => {
  it("isolates production and personal stages", () => {
    expect(relayResourceNameForStage("coddy-studio-relay-traces", "prod")).toBe(
      "coddy-studio-relay-traces-prod",
    );
    expect(relayResourceNameForStage("coddy-studio-relay-traces", "dev_julius")).toBe(
      "coddy-studio-relay-traces-dev-julius",
    );
  });
});

describe("managed endpoint names", () => {
  it("uses the stage slug and a stable stage-scoped digest suffix", () => {
    const hash = "ABCDEF0123456789ABCDEF0123456789";

    expect(managedEndpointDigestInput("dev_julius", "user_123", "env_123")).toBe(
      "dev_julius:user_123:env_123",
    );
    expect(managedEndpointHostname("dev_julius", ".example.com.", hash)).toBe(
      "dev-julius-abcdef0123456789.example.com",
    );
    expect(managedEndpointHostname("prod", "coddy-studio-relay.com", hash)).toBe(
      "prod-abcdef0123456789.coddy-studio-relay.com",
    );
    expect(managedEndpointTunnelName("dev_julius", hash)).toBe(
      "coddy-studio-relay-managedendpoint-dev-julius-abcdef0123456789",
    );
  });

  it("keeps the DNS label within the provider limit for long stage names", () => {
    const hostname = managedEndpointHostname(
      "dev_" + "x".repeat(100),
      "example.com",
      "a".repeat(64),
    );

    expect(hostname.split(".")[0]?.length).toBeLessThanOrEqual(63);
    expect(hostname).toMatch(/-a{16}\.example\.com$/);
  });

  it("accepts allocated hostnames within the relay zone", () => {
    expect(
      isManagedEndpointHostname("dev-julius-abcdef0123456789.example.com", "example.com"),
    ).toBe(true);
    expect(managedEndpointForHostname("dev-julius-abcdef0123456789.example.com")).toEqual({
      httpBaseUrl: "https://dev-julius-abcdef0123456789.example.com/",
      wsBaseUrl: "wss://dev-julius-abcdef0123456789.example.com/ws",
      providerKind: "cloudflare_tunnel",
    });
  });

  it("rejects hostnames outside the relay zone", () => {
    expect(isManagedEndpointHostname("internal.example.net", "example.com")).toBe(false);
    expect(isManagedEndpointHostname("example.com.attacker.test", "example.com")).toBe(false);
    expect(isManagedEndpointHostname("dev-julius.example.com.", "example.com")).toBe(false);
  });
});

describe("managedEndpointPreviewHostnames", () => {
  it("appends -p1 … -p4 to the environment label in the same zone", () => {
    expect(managedEndpointPreviewHostnames("dev-julius-0123456789abcdef.coddy.test")).toEqual([
      "dev-julius-0123456789abcdef-p1.coddy.test",
      "dev-julius-0123456789abcdef-p2.coddy.test",
      "dev-julius-0123456789abcdef-p3.coddy.test",
      "dev-julius-0123456789abcdef-p4.coddy.test",
    ]);
  });

  it("keeps a 60-character label whole and shortens the stage prefix of a 61-character one", () => {
    const hash = "0123456789abcdef";
    const sixty = `${"s".repeat(43)}-${hash}`;
    const sixtyOne = `${"s".repeat(44)}-${hash}`;
    expect(sixty).toHaveLength(60);
    expect(sixtyOne).toHaveLength(61);
    const whole = [1, 2, 3, 4].map((slot) => `${sixty}-p${slot}.coddy.test`);
    expect(managedEndpointPreviewHostnames(`${sixty}.coddy.test`)).toEqual(whole);
    expect(managedEndpointPreviewHostnames(`${sixtyOne}.coddy.test`)).toEqual(whole);
    expect(whole[0]!.split(".")[0]).toHaveLength(63);
  });

  it("shortens a full-length environment label to 63 characters", () => {
    const hostname = managedEndpointHostname("s".repeat(80), "coddy.test", "0123456789abcdef");
    expect(hostname.split(".")[0]).toHaveLength(63);
    expect(managedEndpointPreviewHostnames(hostname)).toEqual(
      [1, 2, 3, 4].map((slot) => `${"s".repeat(43)}-0123456789abcdef-p${slot}.coddy.test`),
    );
  });
});
