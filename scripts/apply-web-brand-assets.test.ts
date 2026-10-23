import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import { Command, CliError } from "effect/cli";

import {
  applyWebBrandAssets,
  makeApplyWebBrandAssetsCommand,
  runApplyWebBrandAssetsMain,
} from "./apply-web-brand-assets.ts";
import { resolveWebIconOverrides } from "./lib/brand-assets.ts";

it.layer(NodeServices.layer)("apply-web-brand-assets", (it) => {
  it.effect("copies every selected brand asset into the requested output", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "apply-web-brand-assets-" });

      for (const override of resolveWebIconOverrides("nightly", "site")) {
        const source = path.join(rootDir, override.sourceRelativePath);
        yield* fs.makeDirectory(path.dirname(source), { recursive: true });
        yield* fs.writeFileString(source, override.sourceRelativePath);
      }
      yield* fs.makeDirectory(path.join(rootDir, "site"), { recursive: true });

      yield* applyWebBrandAssets("nightly", "site", { rootDir });

      for (const override of resolveWebIconOverrides("nightly", "site")) {
        assert.equal(
          yield* fs.readFileString(path.join(rootDir, override.targetRelativePath)),
          override.sourceRelativePath,
        );
      }
    }),
  );

  it.effect("surfaces a missing source asset without writing repository files", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "apply-web-brand-assets-error-",
      });
      const error = yield* applyWebBrandAssets("production", "site", { rootDir }).pipe(Effect.flip);

      assert.equal(error._tag, "PlatformError");
    }),
  );

  it.effect("uses the script repository root when no test root override is supplied", () => {
    const copies: Array<readonly [string, string]> = [];
    return Effect.gen(function* () {
      yield* applyWebBrandAssets("development", "output").pipe(
        Effect.provideService(
          FileSystem.FileSystem,
          FileSystem.makeNoop({
            copyFile: (source, target) => {
              copies.push([source, target]);
              return Effect.void;
            },
          }),
        ),
      );

      assert.lengthOf(copies, 4);
      assert.match(copies[0]![0], /[\\/]assets[\\/]prod[\\/]/);
      assert.match(copies[0]![1], /[\\/]output[\\/]/);
    });
  });

  it.effect("resolves defaults, channels, explicit brands, and target directories", () => {
    const calls: Array<readonly [string, string]> = [];
    const command = makeApplyWebBrandAssetsCommand((brand, targetDirectory) => {
      calls.push([brand, targetDirectory]);
      return Effect.void;
    });
    const runCli = Command.runWith(command, { version: "0.0.0" });

    return Effect.gen(function* () {
      yield* runCli([]);
      yield* runCli(["--channel", "nightly"]);
      yield* runCli(["development", "--channel", "latest", "dev-output"]);

      assert.deepStrictEqual(calls, [
        ["production", "apps/web/dist"],
        ["nightly", "apps/web/dist"],
        ["development", "dev-output"],
      ]);
    });
  });

  it.effect("rejects unsupported brand and channel values", () => {
    const runCli = Command.runWith(makeApplyWebBrandAssetsCommand(), { version: "0.0.0" });

    return Effect.gen(function* () {
      for (const args of [["beta"], ["--channel", "beta"]]) {
        const error = yield* runCli(args).pipe(Effect.flip);
        assert.isTrue(CliError.isCliError(error));
      }
    });
  });
});

it("does not launch when imported and launches exactly once as the direct CLI", () => {
  const programs: Array<object> = [];
  const launch = <E, A>(program: Effect.Effect<A, E, never>) => {
    programs.push(program);
  };

  assert.isFalse(runApplyWebBrandAssetsMain(false, launch));
  assert.isTrue(runApplyWebBrandAssetsMain(true, launch));
  assert.lengthOf(programs, 1);
});
