// @effect-diagnostics nodeBuiltinImport:off - The standalone build adapter launches Tauri.
import * as NodeChildProcess from "node:child_process";

import type { DesktopUiPlatform } from "./app-path.ts";
import { requireReleaseTarget, type ReleaseArch } from "../../../../scripts/lib/release-targets.ts";

const bundles: Record<DesktopUiPlatform, string> = {
  linux: "appimage",
  mac: "dmg",
  win: "nsis",
};

export interface PackagedDesktopUiBuildInput {
  readonly platform: DesktopUiPlatform;
  readonly arch: ReleaseArch;
  readonly bundle?: string;
}

export interface PackagedDesktopUiBuildPlan {
  readonly args: ReadonlyArray<string>;
  readonly environment: Readonly<Record<string, string>>;
}

export function planPackagedDesktopUiBuild(
  input: PackagedDesktopUiBuildInput,
): PackagedDesktopUiBuildPlan {
  const bundle = input.bundle ?? bundles[input.platform];
  const rustTarget = requireReleaseTarget(input.platform, input.arch).rustTarget;
  return {
    environment: {
      VITE_CODDY_STUDIO_DESKTOP_E2E: "1",
      ...(input.platform === "linux" ? { NO_STRIP: "1" } : {}),
    },
    args: [
      "../../scripts/run-msvc.mjs",
      "pnpm",
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
    ],
  };
}

function configuredPlatform(): DesktopUiPlatform {
  if (
    process.env.CODDY_STUDIO_E2E_PLATFORM === "linux" ||
    process.env.CODDY_STUDIO_E2E_PLATFORM === "mac" ||
    process.env.CODDY_STUDIO_E2E_PLATFORM === "win"
  ) {
    return process.env.CODDY_STUDIO_E2E_PLATFORM;
  }
  // oxlint-disable-next-line coddy/no-global-process-runtime -- The standalone build CLI selects its native host target.
  return process.platform === "darwin" ? "mac" : process.platform === "win32" ? "win" : "linux";
}

function configuredArch(): ReleaseArch {
  if (process.env.CODDY_STUDIO_E2E_ARCH === "arm64" || process.env.CODDY_STUDIO_E2E_ARCH === "x64") {
    return process.env.CODDY_STUDIO_E2E_ARCH;
  }
  // oxlint-disable-next-line coddy/no-global-process-runtime -- The build CLI resolves its native architecture when CI did not provide one.
  return process.arch === "arm64" ? "arm64" : "x64";
}

function run(): void {
  const plan = planPackagedDesktopUiBuild({
    platform: configuredPlatform(),
    arch: configuredArch(),
    ...(process.env.CODDY_STUDIO_E2E_BUNDLE ? { bundle: process.env.CODDY_STUDIO_E2E_BUNDLE } : {}),
  });
  const result = NodeChildProcess.spawnSync(process.execPath, [...plan.args], {
    env: { ...process.env, ...plan.environment },
    shell: false,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    process.exitCode = result.status ?? 1;
  }
}

if (import.meta.main) run();
