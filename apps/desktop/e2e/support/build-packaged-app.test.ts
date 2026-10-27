import { describe, expect, it } from "vite-plus/test";

import { planPackagedDesktopUiBuild } from "./build-packaged-app.ts";

describe("planPackagedDesktopUiBuild", () => {
  it.each([
    ["mac", "arm64", "dmg", "aarch64-apple-darwin"],
    ["linux", "x64", "appimage", "x86_64-unknown-linux-gnu"],
    ["win", "x64", "nsis", "x86_64-pc-windows-msvc"],
  ] as const)(
    "places the %s bundle and native Rust target before Cargo arguments",
    (platform, arch, bundle, rustTarget) => {
      const plan = planPackagedDesktopUiBuild({ platform, arch });

      expect(plan.args).toEqual(
        expect.arrayContaining([
          "exec",
          "tauri",
          "build",
          "--features",
          "desktop-e2e",
          "--config",
          "./src-tauri/tauri.e2e.conf.json",
          "--bundles",
          bundle,
          "--target",
          rustTarget,
        ]),
      );
      expect(plan.args).not.toContain("--");
      expect(plan.environment).toEqual({
        VITE_CODDY_STUDIO_DESKTOP_E2E: "1",
        ...(platform === "linux" ? { NO_STRIP: "1" } : {}),
      });
    },
  );

  it("selects native ARM64 targets on Linux and Windows", () => {
    expect(planPackagedDesktopUiBuild({ platform: "linux", arch: "arm64" }).args).toContain(
      "aarch64-unknown-linux-gnu",
    );
    expect(planPackagedDesktopUiBuild({ platform: "win", arch: "arm64" }).args).toContain(
      "aarch64-pc-windows-msvc",
    );
  });
});
