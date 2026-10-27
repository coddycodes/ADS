import {
  BearerConnectionTarget,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
  UnavailableConnectionTarget,
  type ConnectionTarget,
} from "@coddy/client-runtime/connection";
import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { desktopLocalConnectionId } from "./desktopLocal";
import {
  createEnvironmentPresentationPolicy,
  environmentConnectionActions,
  normalizeDesktopHostPlatform,
} from "./environmentPresentationPolicy";

const primaryTarget = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("primary"),
  httpBaseUrl: "http://127.0.0.1:3773",
  label: "This device",
  wsBaseUrl: "ws://127.0.0.1:3773",
});

const wslTarget = new BearerConnectionTarget({
  connectionId: desktopLocalConnectionId("wsl:Ubuntu"),
  environmentId: EnvironmentId.make("wsl"),
  label: "WSL: Ubuntu",
  serverEnvironmentId: null,
});

const unavailableWslTarget = new UnavailableConnectionTarget({
  configuredDistro: "Ubuntu",
  connectionId: desktopLocalConnectionId("wsl:Ubuntu"),
  detail: "WSL is unavailable",
  environmentId: EnvironmentId.make("wsl-unavailable"),
  label: "WSL: Ubuntu",
});

const sshTarget = new SshConnectionTarget({
  connectionId: "ssh:server",
  environmentId: EnvironmentId.make("ssh"),
  label: "SSH server",
});

const relayTarget = new RelayConnectionTarget({
  environmentId: EnvironmentId.make("relay"),
  label: "Relay server",
});

const remoteBearerTarget = new BearerConnectionTarget({
  connectionId: "remote:server",
  environmentId: EnvironmentId.make("bearer"),
  label: "Remote server",
  serverEnvironmentId: null,
});

const allTargetKinds: readonly ConnectionTarget[] = [
  primaryTarget,
  wslTarget,
  unavailableWslTarget,
  sshTarget,
  relayTarget,
  remoteBearerTarget,
];

describe("environment presentation policy", () => {
  it.each([
    ["browser", "macos", false, true],
    ["desktop", "macos", false, false],
    ["desktop", "linux", false, false],
    ["desktop", "windows", true, false],
    ["desktop", "unknown", false, false],
  ] as const)("derives %s/%s presentation", (surface, platform, localSettings, remote) => {
    const policy = createEnvironmentPresentationPolicy({ surface, platform });

    expect(policy.showLocalEnvironmentSettings).toBe(localSettings);
    expect(policy.showRemoteDeviceControls).toBe(remote);
  });

  it("shows only primary and Windows desktop-local targets in local-only desktop mode", () => {
    const windows = createEnvironmentPresentationPolicy({
      surface: "desktop",
      platform: "windows",
    });

    expect(windows.presentsTarget(primaryTarget)).toBe(true);
    expect(windows.presentsTarget(wslTarget)).toBe(true);
    expect(windows.presentsTarget(unavailableWslTarget)).toBe(true);
    expect(windows.presentsTarget(sshTarget)).toBe(false);
    expect(windows.presentsTarget(relayTarget)).toBe(false);
    expect(windows.presentsTarget(remoteBearerTarget)).toBe(false);
    expect(windows.permitsConnectionAction(remoteBearerTarget)).toBe(false);
  });

  it.each(["macos", "linux", "unknown"] as const)(
    "rejects WSL and remote targets on %s desktop hosts",
    (platform) => {
      const policy = createEnvironmentPresentationPolicy({ surface: "desktop", platform });

      expect(policy.presentsTarget(primaryTarget)).toBe(true);
      expect(policy.presentsTarget(wslTarget)).toBe(false);
      expect(policy.presentsTarget(unavailableWslTarget)).toBe(false);
      expect(policy.presentsTarget(sshTarget)).toBe(false);
      expect(policy.presentsTarget(relayTarget)).toBe(false);
      expect(policy.presentsTarget(remoteBearerTarget)).toBe(false);
    },
  );

  it("presents every target kind in browser mode", () => {
    const browser = createEnvironmentPresentationPolicy({
      surface: "browser",
      platform: "unknown",
    });

    for (const target of allTargetKinds) {
      expect(browser.presentsTarget(target)).toBe(true);
      expect(browser.permitsConnectionAction(target)).toBe(true);
    }
  });

  it.each([
    ["MacIntel", "macos"],
    ["MacBookPro", "macos"],
    ["Win32", "windows"],
    ["Windows", "windows"],
    ["Linux x86_64", "linux"],
    ["FreeBSD", "unknown"],
  ] as const)("normalizes %s as %s", (platform, expected) => {
    expect(normalizeDesktopHostPlatform(platform)).toBe(expected);
  });
});

describe("environmentConnectionActions", () => {
  const desktop = createEnvironmentPresentationPolicy({ surface: "desktop", platform: "linux" });
  const browser = createEnvironmentPresentationPolicy({ surface: "browser", platform: "linux" });

  it("sends remote targets on desktop to Remote Servers instead of reconnecting in place", () => {
    for (const target of [sshTarget, relayTarget, remoteBearerTarget]) {
      expect(environmentConnectionActions(desktop, target)).toEqual({
        reconnect: false,
        openRemoteServers: true,
      });
    }
  });

  it("reconnects in place wherever this client manages the connection", () => {
    expect(environmentConnectionActions(desktop, primaryTarget)).toEqual({
      reconnect: true,
      openRemoteServers: false,
    });
    for (const target of allTargetKinds) {
      expect(environmentConnectionActions(browser, target)).toEqual({
        reconnect: true,
        openRemoteServers: false,
      });
    }
  });

  it("offers nothing for an unknown target", () => {
    expect(environmentConnectionActions(desktop, null)).toEqual({
      reconnect: false,
      openRemoteServers: false,
    });
  });
});
