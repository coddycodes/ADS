import { describe, expect, it } from "vite-plus/test";

import { findReleaseAsset, type ReleaseAsset } from "./releases.ts";

describe("release asset lookup", () => {
  it("resolves desktop and server ARM64 assets without matching signatures", () => {
    const assets: ReleaseAsset[] = [
      { name: "BiBCode_0.4.3_arm64-setup.exe", browser_download_url: "desktop-win-arm" },
      { name: "BiBCode_0.4.3_arm64-setup.exe.sig", browser_download_url: "signature" },
      {
        name: "bibcode-server-v0.4.3-windows-aarch64.zip",
        browser_download_url: "server-win-arm",
      },
      { name: "bibcode-server_0.4.3_arm64.deb", browser_download_url: "server-deb-arm" },
    ];

    expect(findReleaseAsset(assets, "arm64-setup.exe")?.browser_download_url).toBe(
      "desktop-win-arm",
    );
    expect(findReleaseAsset(assets, "-windows-aarch64.zip")?.browser_download_url).toBe(
      "server-win-arm",
    );
    expect(findReleaseAsset(assets, "_arm64.deb")?.browser_download_url).toBe("server-deb-arm");
  });

  it("never offers a Breakpad symbol archive as a Windows server download", () => {
    // Release assets upload in name order, so both symbol archives precede the server ZIP.
    const assets: ReleaseAsset[] = [
      {
        name: "bibcode-desktop-symbols-0.4.3-windows-x86_64.zip",
        browser_download_url: "desktop-symbols",
      },
      {
        name: "bibcode-server-symbols-0.4.3-windows-x86_64.zip",
        browser_download_url: "server-symbols",
      },
      { name: "bibcode-server-v0.4.3-windows-x86_64.zip", browser_download_url: "server-win-x64" },
    ];

    expect(findReleaseAsset(assets, "-windows-x86_64.zip")?.browser_download_url).toBe(
      "server-win-x64",
    );
    expect(findReleaseAsset(assets.slice(0, 2), "-windows-x86_64.zip")).toBeUndefined();
  });

  it("returns no match for internal metadata and signatures", () => {
    const assets: ReleaseAsset[] = [
      { name: "payload.minisig", browser_download_url: "minisig" },
      { name: "payload.sig", browser_download_url: "tauri-signature" },
      { name: "payload.sbom.json", browser_download_url: "sbom" },
      { name: "bibcode-server-SHA256SUMS", browser_download_url: "checksums" },
    ];

    expect(findReleaseAsset(assets, ".sig")).toBeUndefined();
    expect(findReleaseAsset(assets, "SHA256SUMS")).toBeUndefined();
  });
});
