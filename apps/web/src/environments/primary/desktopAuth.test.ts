import type { DesktopBridge } from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "@effect/vitest";

import { readDesktopPrimaryBearerToken } from "./desktopAuth";

describe("desktop primary auth", () => {
  beforeEach(() => {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {},
    });
  });

  afterEach(() => {
    Reflect.deleteProperty(globalThis, "window");
  });

  it("preserves bridge failures and permits the bridge's next successful read", async () => {
    const failure = new Error("Bearer exchange failed.");
    const getLocalEnvironmentBearerToken = vi
      .fn()
      .mockRejectedValueOnce(failure)
      .mockResolvedValue("recovered-bearer-token");
    window.desktopBridge = {
      getLocalEnvironmentBearerToken,
    } as unknown as DesktopBridge;

    await expect(readDesktopPrimaryBearerToken()).rejects.toBe(failure);
    await expect(readDesktopPrimaryBearerToken()).resolves.toBe("recovered-bearer-token");
  });

  it("does not require desktop auth in a browser", async () => {
    await expect(readDesktopPrimaryBearerToken()).resolves.toBeNull();
  });

  it("does not require a browser global", async () => {
    Reflect.deleteProperty(globalThis, "window");
    await expect(readDesktopPrimaryBearerToken()).resolves.toBeNull();
  });
});
