import type { ServerProvider, ServerProviderVersionAdvisory } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  getProviderSummary,
  getProviderVersionAdvisoryPresentation,
  getProviderVersionLabel,
} from "./providerStatus";

function provider(overrides: Partial<ServerProvider> = {}): ServerProvider {
  return {
    enabled: true,
    installed: true,
    status: "ready",
    auth: { status: "unknown" },
    message: undefined,
    ...overrides,
  } as ServerProvider;
}

function advisory(
  overrides: Partial<ServerProviderVersionAdvisory> = {},
): ServerProviderVersionAdvisory {
  return {
    status: "behind_latest",
    currentVersion: "1.0.0",
    latestVersion: "2.0.0",
    updateCommand: "provider update",
    canUpdate: true,
    checkedAt: null,
    message: null,
    ...overrides,
  };
}

describe("getProviderSummary", () => {
  it("describes an unreported provider", () => {
    expect(getProviderSummary(undefined)).toEqual({
      headline: "Checking provider status",
      detail: "Waiting for the server to report installation and authentication details.",
    });
  });

  it("asks to enable a disabled provider before checking its installation", () => {
    expect(
      getProviderSummary(provider({ enabled: false, installed: false, status: "disabled" })),
    ).toEqual({
      headline: "Disabled",
      detail:
        "This provider is disabled for new sessions in Coddy Studio. Turn it on to check whether it is installed.",
    });
  });

  it("prefers server detail for disabled and missing providers", () => {
    expect(getProviderSummary(provider({ enabled: false, message: "Disabled by policy" }))).toEqual(
      {
        headline: "Disabled",
        detail: "Disabled by policy",
      },
    );
    expect(
      getProviderSummary(
        provider({ enabled: false, installed: false, message: "Disabled by policy" }),
      ),
    ).toEqual({
      headline: "Disabled",
      detail: "Disabled by policy",
    });
    expect(getProviderSummary(provider({ installed: false, message: "Binary missing" }))).toEqual({
      headline: "Not found",
      detail: "Binary missing",
    });
    expect(getProviderSummary(provider({ enabled: false, installed: true }))).toEqual({
      headline: "Disabled",
      detail: "This provider is installed but disabled for new sessions in Coddy Studio.",
    });
    expect(getProviderSummary(provider({ installed: false }))).toEqual({
      headline: "Not found",
      detail: "CLI not detected on PATH.",
    });
  });

  it("formats authenticated and unauthenticated providers", () => {
    expect(
      getProviderSummary(
        provider({ auth: { status: "authenticated", type: "oauth", label: "User account" } }),
      ),
    ).toEqual({ headline: "Authenticated · User account", detail: null });
    expect(
      getProviderSummary(provider({ auth: { status: "authenticated", type: "api-key" } })),
    ).toEqual({ headline: "Authenticated · api-key", detail: null });
    expect(getProviderSummary(provider({ auth: { status: "authenticated" } }))).toEqual({
      headline: "Authenticated",
      detail: null,
    });
    expect(
      getProviderSummary(
        provider({ auth: { status: "unauthenticated" }, message: "Sign in required" }),
      ),
    ).toEqual({ headline: "Not authenticated", detail: "Sign in required" });
  });

  it("describes warning, error, and ready states", () => {
    expect(getProviderSummary(provider({ status: "warning" }))).toMatchObject({
      headline: "Needs attention",
      detail: expect.stringContaining("could not fully verify"),
    });
    expect(getProviderSummary(provider({ status: "error", message: "Probe failed" }))).toEqual({
      headline: "Unavailable",
      detail: "Probe failed",
    });
    expect(getProviderSummary(provider())).toMatchObject({
      headline: "Available",
      detail: expect.stringContaining("authentication could not be verified"),
    });
  });
});

describe("provider version presentation", () => {
  it("normalizes optional version labels", () => {
    expect(getProviderVersionLabel(undefined)).toBeNull();
    expect(getProviderVersionLabel(null)).toBeNull();
    expect(getProviderVersionLabel("v2.0.0")).toBe("v2.0.0");
    expect(getProviderVersionLabel("2.0.0")).toBe("v2.0.0");
  });

  it("hides current and unexplained unknown advisories", () => {
    expect(getProviderVersionAdvisoryPresentation(undefined)).toBeNull();
    expect(getProviderVersionAdvisoryPresentation(advisory({ status: "current" }))).toBeNull();
    expect(
      getProviderVersionAdvisoryPresentation(advisory({ status: "unknown", message: null })),
    ).toBeNull();
  });

  it("presents a failed update check without an update command", () => {
    expect(
      getProviderVersionAdvisoryPresentation(
        advisory({
          status: "unknown",
          message: "Could not check for provider updates.",
          updateCommand: "provider update",
        }),
      ),
    ).toEqual({
      kind: "check_failed",
      title: "Update check failed",
      detail: "Could not check for provider updates.",
      updateCommand: null,
      emphasis: "strong",
    });
  });

  it("uses server, versioned, and generic update detail", () => {
    expect(
      getProviderVersionAdvisoryPresentation(advisory({ message: "Upgrade now" })),
    ).toMatchObject({
      kind: "update",
      title: "Update available",
      detail: "Upgrade now",
      updateCommand: "provider update",
    });
    expect(getProviderVersionAdvisoryPresentation(advisory())).toMatchObject({
      detail: "Update available: install v2.0.0.",
    });
    expect(
      getProviderVersionAdvisoryPresentation(
        advisory({ latestVersion: null, updateCommand: null }),
      ),
    ).toMatchObject({
      detail: "Update available: install the latest provider version.",
      updateCommand: null,
      emphasis: "normal",
    });
  });
});
