import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Config from "effect/Config";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import { Command, CliError } from "effect/cli";
import * as TestConsole from "effect/testing/TestConsole";

import {
  readDesktopBaseVersion,
  isValidNightlyDate,
  resolveNightlyReleaseCommand,
  resolveNightlyBaseVersion,
  resolveNightlyReleaseMetadata,
  resolveNightlyTargetVersion,
  writeNightlyReleaseOutput,
  runResolveNightlyReleaseMain,
} from "./resolve-nightly-release.ts";

it("validates nightly dates as real Gregorian calendar days", () => {
  for (const valid of ["20000229", "20240229", "20261231"]) {
    assert.isTrue(isValidNightlyDate(valid));
  }
  for (const invalid of [
    "00000101",
    "19000229",
    "20230229",
    "20260431",
    "20260010",
    "20261301",
    "20260100",
    "20260132",
    "2026-01-01",
  ]) {
    assert.isFalse(isValidNightlyDate(invalid));
  }
});

it("strips prerelease and build metadata when deriving the nightly base version", () => {
  assert.equal(resolveNightlyBaseVersion("0.0.17"), "0.0.17");
  assert.equal(resolveNightlyBaseVersion("9.9.9-smoke.0"), "9.9.9");
  assert.equal(resolveNightlyBaseVersion("1.2.3-beta.4+build.9"), "1.2.3");
});

it.effect("bumps the patch version before deriving nightly prerelease versions", () =>
  Effect.gen(function* () {
    assert.equal(yield* resolveNightlyTargetVersion("0.0.17"), "0.0.18");
    assert.equal(yield* resolveNightlyTargetVersion("9.9.9-smoke.0"), "9.9.10");
    assert.equal(yield* resolveNightlyTargetVersion("1.2.3-beta.4+build.9"), "1.2.4");
    assert.equal(
      yield* resolveNightlyTargetVersion("1.2.9007199254740993"),
      "1.2.9007199254740994",
    );
  }),
);

it.effect("rejects malformed SemVer desktop package versions", () =>
  Effect.gen(function* () {
    for (const version of [
      "01.2.3",
      "1.02.3",
      "1.2.03",
      "1.2.3-01",
      "1.2.3-alpha..1",
      "1.2.3+",
      "1.2.3-caf\u00e9",
    ]) {
      const error = yield* resolveNightlyTargetVersion(version).pipe(Effect.flip);
      assert.equal(error._tag, "InvalidDesktopPackageVersionError");
      assert.equal(error.version, version);
    }
  }),
);

it.effect("reports the invalid desktop package version", () =>
  Effect.gen(function* () {
    const error = yield* resolveNightlyTargetVersion("nightly").pipe(Effect.flip);

    assert.equal(error._tag, "InvalidDesktopPackageVersionError");
    assert.equal(error.version, "nightly");
    assert.equal(error.message, "Invalid desktop package version 'nightly'.");
  }),
);

it("derives nightly metadata including the short commit sha in the release name", () => {
  assert.deepStrictEqual(
    resolveNightlyReleaseMetadata("9.9.10", "20260413", 321, "abcdef1234567890"),
    {
      baseVersion: "9.9.10",
      version: "9.9.10-nightly.20260413.321",
      tag: "v9.9.10-nightly.20260413.321",
      name: "Coddy Studio Nightly 9.9.10-nightly.20260413.321 (abcdef123456)",
      shortSha: "abcdef123456",
    },
  );
});

it.effect("preserves the GITHUB_OUTPUT configuration cause", () => {
  const metadata = resolveNightlyReleaseMetadata("1.2.4", "20260620", 42, "abcdef1234567890");
  const configCause = new ConfigProvider.SourceError({ message: "environment unavailable" });

  return Effect.gen(function* () {
    const configError = yield* writeNightlyReleaseOutput(metadata, true).pipe(
      Effect.provideService(FileSystem.FileSystem, FileSystem.makeNoop({})),
      Effect.provideService(
        ConfigProvider.ConfigProvider,
        ConfigProvider.make(() => Effect.fail(configCause)),
      ),
      Effect.flip,
    );

    if (configError._tag !== "NightlyReleaseGitHubOutputConfigError") {
      return assert.fail(`Unexpected error: ${configError._tag}`);
    }
    assert.instanceOf(configError.cause, Config.ConfigError);
    assert.strictEqual(configError.cause.cause, configCause);
    assert.notInclude(configError.message, configCause.message);
  });
});

it.layer(NodeServices.layer)("readDesktopBaseVersion", (it) => {
  it.effect("reads and bumps a valid desktop package version", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "resolve-nightly-success-" });
      const packageJsonPath = path.join(rootDir, "apps/desktop/package.json");
      yield* fs.makeDirectory(path.dirname(packageJsonPath), { recursive: true });
      yield* fs.writeFileString(packageJsonPath, '{"version":"1.2.3-beta.1"}');

      assert.equal(yield* readDesktopBaseVersion(rootDir), "1.2.4");
    }),
  );

  it.effect("resolves the repository root when no override is supplied", () =>
    Effect.gen(function* () {
      const reads: Array<string> = [];
      const version = yield* readDesktopBaseVersion(undefined).pipe(
        Effect.provideService(
          FileSystem.FileSystem,
          FileSystem.makeNoop({
            readFileString: (filePath) => {
              reads.push(String(filePath));
              return Effect.succeed('{"version":"4.5.6"}');
            },
          }),
        ),
      );

      assert.equal(version, "4.5.7");
      assert.match(reads[0]!, /[\\/]apps[\\/]desktop[\\/]package\.json$/);
    }),
  );

  it.effect("preserves desktop package read context and its platform cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "resolve-nightly-release-read-",
      });
      const packageJsonPath = path.join(rootDir, "apps/desktop/package.json");

      const error = yield* readDesktopBaseVersion(rootDir).pipe(Effect.flip);

      if (error._tag !== "NightlyReleaseDesktopPackageError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "read");
      assert.equal(error.packageJsonPath, packageJsonPath);
      assert.instanceOf(error.cause, PlatformError.PlatformError);
      assert.notInclude(error.message, String((error.cause as Error).message));
    }),
  );

  it.effect("preserves desktop package decode context and its schema cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "resolve-nightly-release-decode-",
      });
      const packageJsonPath = path.join(rootDir, "apps/desktop/package.json");
      yield* fs.makeDirectory(path.dirname(packageJsonPath), { recursive: true });
      yield* fs.writeFileString(packageJsonPath, "{");

      const error = yield* readDesktopBaseVersion(rootDir).pipe(Effect.flip);

      if (error._tag !== "NightlyReleaseDesktopPackageError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "decode");
      assert.equal(error.packageJsonPath, packageJsonPath);
      assert.ok(error.cause !== undefined);
      assert.notInclude(error.message, String((error.cause as Error).message));
    }),
  );
});

it.layer(NodeServices.layer)("writeNightlyReleaseOutput", (it) => {
  const metadata = resolveNightlyReleaseMetadata("1.2.4", "20260620", 42, "abcdef1234567890");

  it.effect("prints every output entry when GitHub output is disabled", () =>
    Effect.gen(function* () {
      yield* writeNightlyReleaseOutput(metadata, false).pipe(Effect.provide(TestConsole.layer));
      assert.deepStrictEqual(yield* TestConsole.logLines, [
        "base_version=1.2.4",
        "version=1.2.4-nightly.20260620.42",
        "tag=v1.2.4-nightly.20260620.42",
        "name=Coddy Studio Nightly 1.2.4-nightly.20260620.42 (abcdef123456)",
        "short_sha=abcdef123456",
      ]);
    }).pipe(Effect.provide(TestConsole.layer)),
  );

  it.effect("appends every output entry to GITHUB_OUTPUT", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const outputDir = yield* fs.makeTempDirectoryScoped({ prefix: "nightly-output-" });
      const outputPath = `${outputDir}/github-output.txt`;
      yield* writeNightlyReleaseOutput(metadata, true).pipe(
        Effect.provide(
          ConfigProvider.layer(ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } })),
        ),
      );
      assert.equal(
        yield* fs.readFileString(outputPath),
        "base_version=1.2.4\nversion=1.2.4-nightly.20260620.42\n" +
          "tag=v1.2.4-nightly.20260620.42\n" +
          "name=Coddy Studio Nightly 1.2.4-nightly.20260620.42 (abcdef123456)\n" +
          "short_sha=abcdef123456\n",
      );
    }),
  );

  it.effect("preserves GITHUB_OUTPUT append failures", () => {
    const outputPath = "/nightly-output";
    const cause = PlatformError.systemError({
      _tag: "PermissionDenied",
      module: "FileSystem",
      method: "writeFileString",
      pathOrDescriptor: outputPath,
    });
    return Effect.gen(function* () {
      const error = yield* writeNightlyReleaseOutput(metadata, true).pipe(
        Effect.provideService(
          FileSystem.FileSystem,
          FileSystem.makeNoop({ writeFileString: () => Effect.fail(cause) }),
        ),
        Effect.provide(
          ConfigProvider.layer(ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } })),
        ),
        Effect.flip,
      );
      assert.equal(error._tag, "NightlyReleaseGitHubOutputAppendError");
      if (error._tag !== "NightlyReleaseGitHubOutputAppendError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.outputPath, outputPath);
      assert.strictEqual(error.cause, cause);
      assert.equal(error.message, `Failed to append nightly release metadata to ${outputPath}.`);
    });
  });
});

it.layer(NodeServices.layer)("resolve-nightly-release CLI", (it) => {
  const runCli = Command.runWith(resolveNightlyReleaseCommand, { version: "0.0.0" });

  it.effect("resolves and prints nightly metadata through the public command", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "nightly-cli-" });
      const packagePath = path.join(rootDir, "apps/desktop/package.json");
      yield* fs.makeDirectory(path.dirname(packagePath), { recursive: true });
      yield* fs.writeFileString(packagePath, '{"version":"2.3.4"}');

      yield* runCli([
        "--date",
        "20260714",
        "--run-number",
        "9",
        "--sha",
        "abcdef1234567890",
        "--root",
        rootDir,
      ]).pipe(Effect.provide(TestConsole.layer));
      assert.include((yield* TestConsole.logLines).join("\n"), "version=2.3.5-nightly.20260714.9");
    }).pipe(Effect.provide(TestConsole.layer)),
  );

  it.effect("rejects malformed date, run number, and sha flags", () =>
    Effect.gen(function* () {
      for (const args of [
        ["--date", "2026-07-14", "--run-number", "1", "--sha", "abcdef1"],
        ["--date", "20260714", "--run-number", "0", "--sha", "abcdef1"],
        ["--date", "20260714", "--run-number", "1", "--sha", "xyz"],
      ]) {
        assert.isTrue(CliError.isCliError(yield* runCli(args).pipe(Effect.flip)));
      }
    }),
  );
});

it("does not launch on import and launches once for direct execution", () => {
  const programs: Array<object> = [];
  const launch = <E, A>(program: Effect.Effect<A, E, never>) => programs.push(program);

  assert.isFalse(runResolveNightlyReleaseMain(false, launch));
  assert.isTrue(runResolveNightlyReleaseMain(true, launch));
  assert.lengthOf(programs, 1);
});
