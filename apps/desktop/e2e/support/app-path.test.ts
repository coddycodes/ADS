import { describe, expect, it } from "vite-plus/test";

import { DesktopAppPathConfigurationError, resolveDesktopAppPath } from "./app-path.ts";

describe("resolveDesktopAppPath", () => {
  it("resolves the executable inside a macOS app installed from a DMG", () => {
    expect(
      resolveDesktopAppPath({
        platform: "mac",
        environment: {
          CODDY_STUDIO_E2E_APP_PATH: "/Volumes/Coddy Studio/Coddy Studio.app",
        },
      }),
    ).toBe("/Volumes/Coddy Studio/Coddy Studio.app/Contents/MacOS/coddy-studio-desktop");
  });

  it("accepts a direct macOS application executable", () => {
    const executable = "/private/tmp/Coddy Studio.app/Contents/MacOS/Coddy Studio";

    expect(
      resolveDesktopAppPath({
        platform: "mac",
        environment: { CODDY_STUDIO_E2E_APP_PATH: executable },
      }),
    ).toBe(executable);
  });

  it("resolves a Linux AppImage without changing paths that contain spaces", () => {
    const appImage = "/tmp/Coddy Studio UI Smoke/Coddy Studio_0.2.2_amd64.AppImage";

    expect(
      resolveDesktopAppPath({
        platform: "linux",
        environment: { CODDY_STUDIO_E2E_APP_PATH: appImage },
      }),
    ).toBe(appImage);
  });

  it("resolves an NSIS-installed Windows executable with win32 path rules", () => {
    const executable = String.raw`C:\Program Files\Coddy Studio\Coddy Studio.exe`;

    expect(
      resolveDesktopAppPath({
        platform: "win",
        environment: { CODDY_STUDIO_E2E_APP_PATH: executable },
      }),
    ).toBe(executable);
  });

  it("rejects a missing CODDY_STUDIO_E2E_APP_PATH", () => {
    expect(() =>
      resolveDesktopAppPath({
        platform: "linux",
        environment: {},
      }),
    ).toThrowError(DesktopAppPathConfigurationError);
  });

  it("rejects installer paths that are not directly launchable", () => {
    expect(() =>
      resolveDesktopAppPath({
        platform: "mac",
        environment: { CODDY_STUDIO_E2E_APP_PATH: "/tmp/Coddy Studio.dmg" },
      }),
    ).toThrowError(/copy the app from the DMG/i);

    expect(() =>
      resolveDesktopAppPath({
        platform: "win",
        environment: { CODDY_STUDIO_E2E_APP_PATH: String.raw`C:\tmp\Coddy Studio-setup.msi` },
      }),
    ).toThrowError(/installed executable/i);
  });
});
