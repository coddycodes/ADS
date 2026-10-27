import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import {
  formatAppDisplayName,
  resolveServerBackedAppDisplayName,
  resolveServerBackedAppStageLabel,
} from "./branding.logic";

const originalWindow = globalThis.window;

afterEach(() => {
  vi.resetModules();

  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
    return;
  }

  globalThis.window = originalWindow;
});

describe("branding", () => {
  it("falls back when the desktop bridge has no branding", async () => {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {
        desktopBridge: {
          getAppBranding: () => undefined,
        },
      },
    });

    const branding = await import("./branding");

    expect(branding.APP_BASE_NAME).toBe("Coddy Studio");
  });

  it("uses injected desktop branding when available", async () => {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {
        desktopBridge: {
          getAppBranding: () => ({
            baseName: "Coddy Studio",
            stageLabel: "Nightly",
            displayName: "Coddy Studio (Nightly)",
          }),
        },
      },
    });

    const branding = await import("./branding");

    expect(branding.APP_BASE_NAME).toBe("Coddy Studio");
    expect(branding.APP_STAGE_LABEL).toBe("Nightly");
    expect(branding.APP_DISPLAY_NAME).toBe("Coddy Studio (Nightly)");
  });

  it("normalizes hosted app channel metadata", async () => {
    vi.stubEnv("VITE_HOSTED_APP_CHANNEL", "nightly");

    const branding = await import("./branding");

    expect(branding.HOSTED_APP_CHANNEL).toBe("nightly");
    expect(branding.HOSTED_APP_CHANNEL_LABEL).toBe("Nightly");
    expect(branding.APP_STAGE_LABEL).toBe("Nightly");
    expect(branding.APP_DISPLAY_NAME).toBe("Coddy Studio (Nightly)");
  });

  it("labels the latest hosted app channel", async () => {
    vi.stubEnv("VITE_HOSTED_APP_CHANNEL", "latest");

    const branding = await import("./branding");

    expect(branding.HOSTED_APP_CHANNEL).toBe("latest");
    expect(branding.HOSTED_APP_CHANNEL_LABEL).toBe("Latest");
  });

  it("ignores unknown hosted app channels", async () => {
    vi.stubEnv("VITE_HOSTED_APP_CHANNEL", "preview");

    const branding = await import("./branding");

    expect(branding.HOSTED_APP_CHANNEL).toBeNull();
    expect(branding.HOSTED_APP_CHANNEL_LABEL).toBeNull();
  });
});

describe("branding logic", () => {
  it("omits the channel suffix for stable releases", () => {
    expect(formatAppDisplayName({ baseName: "Coddy Studio", stageLabel: "Latest" })).toBe("Coddy Studio");
  });

  it("keeps the channel suffix for non-stable releases", () => {
    expect(formatAppDisplayName({ baseName: "Coddy Studio", stageLabel: "Dev" })).toBe("Coddy Studio (Dev)");
    expect(formatAppDisplayName({ baseName: "Coddy Studio", stageLabel: "Nightly" })).toBe(
      "Coddy Studio (Nightly)",
    );
  });

  it("returns Nightly for nightly primary server versions", () => {
    expect(
      resolveServerBackedAppStageLabel({
        primaryServerVersion: "0.0.28-nightly.20260616.12",
        fallbackStageLabel: "Latest",
      }),
    ).toBe("Nightly");
  });

  it("updates the display name for nightly primary server versions", () => {
    expect(
      resolveServerBackedAppDisplayName({
        baseName: "Coddy Studio",
        fallbackDisplayName: "Coddy Studio",
        fallbackStageLabel: "Latest",
        primaryServerVersion: "0.0.28-nightly.20260616.12",
      }),
    ).toBe("Coddy Studio (Nightly)");
  });

  it("keeps the fallback display name for stable primary server versions", () => {
    expect(
      resolveServerBackedAppDisplayName({
        baseName: "Coddy Studio",
        fallbackDisplayName: "Coddy Studio",
        fallbackStageLabel: "Latest",
        primaryServerVersion: "0.0.27",
      }),
    ).toBe("Coddy Studio");
  });

  it("keeps the fallback display name for malformed nightly primary server versions", () => {
    expect(
      resolveServerBackedAppDisplayName({
        baseName: "Coddy Studio",
        fallbackDisplayName: "Coddy Studio",
        fallbackStageLabel: "Latest",
        primaryServerVersion: "0.0.28-nightly.20260616",
      }),
    ).toBe("Coddy Studio");
  });
});
