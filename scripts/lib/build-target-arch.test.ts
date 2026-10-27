import { assert, describe, it } from "@effect/vitest";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { HostProcessArchitecture, HostProcessPlatform } from "@coddy/shared/hostProcess";

import { getDefaultBuildArch } from "./build-target-arch.ts";

const compactEnv = (env: Readonly<Record<string, string | undefined>>): Record<string, string> =>
  Object.fromEntries(
    Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );

const withHostRuntime = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
  env: Readonly<Record<string, string | undefined>> = {},
) =>
  Effect.provide(
    Layer.mergeAll(
      Layer.succeed(HostProcessPlatform, platform),
      Layer.succeed(HostProcessArchitecture, arch),
      ConfigProvider.layer(ConfigProvider.fromEnv({ env: compactEnv(env) })),
    ),
  );

describe("build-target-arch", () => {
  it.effect("uses the resolved host arch when selecting the default Windows build arch", () =>
    Effect.gen(function* () {
      // This mirrors the packaging script's default-path behavior: the current
      // process is x64, but the machine itself is ARM64, so the default build
      // target should be win-arm64 rather than win-x64.
      const arch = yield* getDefaultBuildArch("win", { archChoices: ["x64", "arm64"] }).pipe(
        withHostRuntime("win32", "x64", {
          PROCESSOR_ARCHITECTURE: "AMD64", // The currently running Node process is x64.
          PROCESSOR_ARCHITEW6432: "ARM64", // The process is x64, but the actual Windows host is ARM64.
        }),
      );

      assert.equal(arch, "arm64");
    }),
  );

  it.effect("does not apply Windows host env heuristics for non-Windows targets", () =>
    Effect.gen(function* () {
      const arch = yield* getDefaultBuildArch("linux", { archChoices: ["x64", "arm64"] }).pipe(
        withHostRuntime("linux", "x64", {
          PROCESSOR_ARCHITECTURE: "AMD64",
          PROCESSOR_ARCHITEW6432: "ARM64",
        }),
      );

      assert.equal(arch, "x64");
    }),
  );

  it.effect("uses a native ARM64 process without consulting Windows emulation metadata", () =>
    Effect.gen(function* () {
      const arch = yield* getDefaultBuildArch("win", { archChoices: ["x64", "arm64"] }).pipe(
        withHostRuntime("win32", "arm64", {
          PROCESSOR_ARCHITECTURE: "AMD64",
        }),
      );

      assert.equal(arch, "arm64");
    }),
  );

  it.effect("selects each native Darwin architecture when the target supports it", () =>
    Effect.gen(function* () {
      const arm64 = yield* getDefaultBuildArch("mac", { archChoices: ["x64", "arm64"] }).pipe(
        withHostRuntime("darwin", "arm64"),
      );
      const x64 = yield* getDefaultBuildArch("mac", { archChoices: ["arm64", "x64"] }).pipe(
        withHostRuntime("darwin", "x64"),
      );

      assert.equal(arm64, "arm64");
      assert.equal(x64, "x64");
    }),
  );

  it.effect("recognizes each supported Windows processor architecture spelling", () =>
    Effect.gen(function* () {
      const cases = [
        ["ARM64", "arm64"],
        [" aarch64 ", "arm64"],
        ["ARM64-v8", "arm64"],
        ["AMD64", "x64"],
        ["x64", "x64"],
        ["AMD64 Family 25", "x64"],
        ["x64-based PC", "x64"],
      ] as const;

      for (const [processorArchitecture, expected] of cases) {
        const arch = yield* getDefaultBuildArch("win", { archChoices: ["x64", "arm64"] }).pipe(
          withHostRuntime("win32", "x64", { PROCESSOR_ARCHITECTURE: processorArchitecture }),
        );
        assert.equal(arch, expected);
      }
    }),
  );

  it.effect("prefers WOW64 host metadata over the emulated process architecture", () =>
    Effect.gen(function* () {
      const arch = yield* getDefaultBuildArch("win", { archChoices: ["arm64", "x64"] }).pipe(
        withHostRuntime("win32", "x64", {
          PROCESSOR_ARCHITECTURE: "ARM64",
          PROCESSOR_ARCHITEW6432: "AMD64",
        }),
      );

      assert.equal(arch, "x64");
    }),
  );

  it.effect("falls back when the host architecture is unknown or unavailable", () =>
    Effect.gen(function* () {
      const unknownWindows = yield* getDefaultBuildArch("win", {
        archChoices: ["arm64", "x64"],
      }).pipe(
        withHostRuntime("win32", "x64", {
          PROCESSOR_ARCHITECTURE: "mips",
          PROCESSOR_ARCHITEW6432: "   ",
        }),
      );
      const unsupportedProcess = yield* getDefaultBuildArch("linux", {
        archChoices: ["universal", "x64"],
      }).pipe(withHostRuntime("linux", "ia32"));
      const excludedDarwinArm = yield* getDefaultBuildArch("mac", {
        archChoices: ["x64"],
      }).pipe(withHostRuntime("darwin", "arm64"));
      const excludedDarwinX64 = yield* getDefaultBuildArch("mac", {
        archChoices: ["arm64"],
      }).pipe(withHostRuntime("darwin", "x64"));
      const noChoices = yield* getDefaultBuildArch("linux", { archChoices: [] }).pipe(
        withHostRuntime("linux", "ia32"),
      );

      assert.equal(unknownWindows, "x64");
      assert.equal(unsupportedProcess, "universal");
      assert.equal(excludedDarwinArm, "x64");
      assert.equal(excludedDarwinX64, "arm64");
      assert.equal(noChoices, "x64");
    }),
  );
});
