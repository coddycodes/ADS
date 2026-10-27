import { describe, expect, it } from "vite-plus/test";

import {
  BRAND_ASSET_PATHS,
  DEVELOPMENT_ICON_OVERRIDES,
  PUBLISH_ICON_OVERRIDES,
  resolveWebAssetBrandForChannel,
  resolveWebIconOverrides,
  WEB_ASSET_CHANNELS,
} from "./brand-assets.ts";

describe("brand-assets", () => {
  it("publishes the canonical desktop and web asset paths", () => {
    expect(BRAND_ASSET_PATHS).toEqual({
      macIconPng: "assets/prod/coddy-studio-macos-1024.png",
      linuxIconPng: "assets/prod/coddy-studio-universal-1024.png",
      macIconIcns: "assets/prod/coddy-studio-macos.icns",
      windowsIconIco: "assets/prod/coddy-studio-windows.ico",
      webFaviconIco: "assets/prod/coddy-studio-web-favicon.ico",
      webFavicon16Png: "assets/prod/coddy-studio-web-favicon-16x16.png",
      webFavicon32Png: "assets/prod/coddy-studio-web-favicon-32x32.png",
      webAppleTouchIconPng: "assets/prod/coddy-studio-web-apple-touch-180.png",
    });
  });

  it("maps every brand to the canonical web icons", () => {
    const expected = [
      {
        sourceRelativePath: "assets/prod/coddy-studio-web-favicon.ico",
        targetRelativePath: "dist/client/favicon.ico",
      },
      {
        sourceRelativePath: "assets/prod/coddy-studio-web-favicon-16x16.png",
        targetRelativePath: "dist/client/favicon-16x16.png",
      },
      {
        sourceRelativePath: "assets/prod/coddy-studio-web-favicon-32x32.png",
        targetRelativePath: "dist/client/favicon-32x32.png",
      },
      {
        sourceRelativePath: "assets/prod/coddy-studio-web-apple-touch-180.png",
        targetRelativePath: "dist/client/apple-touch-icon.png",
      },
    ];

    for (const brand of ["development", "nightly", "production"] as const) {
      expect(resolveWebIconOverrides(brand, "dist/client")).toEqual(expected);
    }
    expect(DEVELOPMENT_ICON_OVERRIDES).toEqual(expected);
    expect(PUBLISH_ICON_OVERRIDES).toEqual(expected);
  });

  it("maps only the supported release channels and rejects unsupported runtime input", () => {
    expect(WEB_ASSET_CHANNELS).toEqual(["latest", "nightly"]);
    expect(resolveWebAssetBrandForChannel("latest")).toBe("production");
    expect(resolveWebAssetBrandForChannel("nightly")).toBe("nightly");
    expect(() => resolveWebAssetBrandForChannel("beta" as never)).toThrowError(
      "Unsupported web asset channel: beta",
    );
  });
});
