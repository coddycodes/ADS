import { EnvironmentId } from "@bibcode/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { forgetPreviewStorage } from "./previewStorageCleanup";

const forgetEnvironment = vi.fn<(environmentId: string) => Promise<void>>();

beforeEach(() => {
  forgetEnvironment.mockReset().mockResolvedValue(undefined);
  // Installed after this module was imported, as the desktop bridge is at startup.
  vi.stubGlobal("window", { desktopBridge: { preview: { forgetEnvironment } } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("forgetPreviewStorage", () => {
  it("asks the desktop to delete a removed environment's preview storage", async () => {
    await forgetPreviewStorage(EnvironmentId.make("env-gone"));
    expect(forgetEnvironment).toHaveBeenCalledExactlyOnceWith("env-gone");
  });

  it("does nothing without a desktop preview host", async () => {
    vi.stubGlobal("window", {});
    await expect(forgetPreviewStorage(EnvironmentId.make("env-gone"))).resolves.toBeUndefined();
  });

  it("never fails the removal that triggered it", async () => {
    forgetEnvironment.mockRejectedValueOnce(new Error("locked"));
    await expect(forgetPreviewStorage(EnvironmentId.make("env-gone"))).resolves.toBeUndefined();
  });
});
