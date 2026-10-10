import { describe, expect, it, vi } from "vite-plus/test";

import { runTauriBuild, tauriBuildEnvironment } from "./run-tauri-build.mjs";

describe("run Tauri build", () => {
  it("disables linuxdeploy stripping only for the Linux child process", () => {
    const inherited = { SENTINEL: "kept" };

    expect(tauriBuildEnvironment("linux", inherited)).toEqual({
      SENTINEL: "kept",
      NO_STRIP: "1",
    });
    expect(tauriBuildEnvironment("darwin", inherited)).toBe(inherited);
    expect(tauriBuildEnvironment("win32", inherited)).toBe(inherited);
  });

  it("runs the locked Tauri CLI through the platform toolchain adapter", () => {
    const runMsvc = vi.fn(() => 17);

    expect(
      runTauriBuild({
        platform: "linux",
        env: { SENTINEL: "kept" },
        args: ["--features", "desktop-e2e"],
        runMsvc,
      }),
    ).toBe(17);
    expect(runMsvc).toHaveBeenCalledWith(
      ["pnpm", "exec", "tauri", "build", "--features", "desktop-e2e"],
      {
        env: { SENTINEL: "kept", NO_STRIP: "1" },
      },
    );
  });

  it("bundles an already built app with the same toolchain and environment", () => {
    const runMsvc = vi.fn(() => 0);

    runTauriBuild({
      platform: "linux",
      env: {},
      args: ["bundle", "--bundles", "appimage"],
      runMsvc,
    });
    expect(runMsvc).toHaveBeenCalledWith(
      ["pnpm", "exec", "tauri", "bundle", "--bundles", "appimage"],
      { env: { NO_STRIP: "1" } },
    );
  });
});
