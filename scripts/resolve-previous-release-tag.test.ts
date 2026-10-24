import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Config from "effect/Config";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { ChildProcessSpawner } from "effect/process";
import { Command } from "effect/cli";

import {
  listGitTags,
  resolvePreviousReleaseTagCommand,
  resolvePreviousReleaseTag,
  runResolvePreviousReleaseTagMain,
  writePreviousReleaseTagOutput,
} from "./resolve-previous-release-tag.ts";

const encoder = new TextEncoder();

function mockHandle(options: {
  readonly exitCode: number;
  readonly stdout?: string;
  readonly stderr?: string;
  readonly stdoutError?: PlatformError.PlatformError;
  readonly stderrError?: PlatformError.PlatformError;
  readonly exitError?: PlatformError.PlatformError;
}) {
  return ChildProcessSpawner.makeHandle({
    pid: ChildProcessSpawner.ProcessId(1),
    exitCode: options.exitError
      ? Effect.fail(options.exitError)
      : Effect.succeed(ChildProcessSpawner.ExitCode(options.exitCode)),
    isRunning: Effect.succeed(false),
    kill: () => Effect.void,
    unref: Effect.succeed(Effect.void),
    stdin: Sink.drain,
    stdout: options.stdoutError
      ? Stream.fail(options.stdoutError)
      : Stream.make(encoder.encode(options.stdout ?? "")),
    stderr: options.stderrError
      ? Stream.fail(options.stderrError)
      : Stream.make(encoder.encode(options.stderr ?? "")),
    all: Stream.empty,
    getInputFd: () => Sink.drain,
    getOutputFd: () => Stream.empty,
  });
}

it.effect("selects the latest earlier stable tag and ignores nightlies", () =>
  Effect.gen(function* () {
    const previous = yield* resolvePreviousReleaseTag("stable", "v1.2.0", [
      "v1.1.0",
      "v1.1.1-nightly.20260619.1",
      "v1.1.2",
      "v1.2.0",
    ]);

    assert.equal(previous, "v1.1.2");
  }),
);

it.effect("accepts legacy nightly tags when selecting the previous nightly", () =>
  Effect.gen(function* () {
    const previous = yield* resolvePreviousReleaseTag("nightly", "v1.2.0-nightly.20260620.2", [
      "nightly-v1.2.0-nightly.20260620.1",
      "v1.1.0-nightly.20260619.9",
    ]);

    assert.equal(previous, "nightly-v1.2.0-nightly.20260620.1");
  }),
);

it.effect("reports the invalid tag with its release channel", () =>
  Effect.gen(function* () {
    const error = yield* resolvePreviousReleaseTag("nightly", "v1.2.0", []).pipe(Effect.flip);

    assert.equal(error._tag, "InvalidReleaseTagError");
    assert.equal(error.channel, "nightly");
    assert.equal(error.currentTag, "v1.2.0");
    assert.equal(error.message, "Invalid nightly release tag 'v1.2.0'.");
  }),
);

it.effect("reports invalid stable tags and returns no candidate when none is earlier", () =>
  Effect.gen(function* () {
    const error = yield* resolvePreviousReleaseTag("stable", "not-a-tag", []).pipe(Effect.flip);
    assert.equal(error._tag, "InvalidReleaseTagError");
    assert.equal(error.message, "Invalid stable release tag 'not-a-tag'.");
    assert.isUndefined(yield* resolvePreviousReleaseTag("stable", "v1.0.0", ["v1.0.0"]));
  }),
);

it.effect("implements stable semantic-version precedence including prereleases", () =>
  Effect.gen(function* () {
    const cases = [
      ["v2.0.0", ["v1.9.9", "v2.0.0"], "v1.9.9"],
      ["v1.2.0", ["v1.1.9", "v1.2.0"], "v1.1.9"],
      ["v1.2.3", ["v1.2.2", "v1.2.3"], "v1.2.2"],
      ["v1.2.3", ["v1.2.3-rc.1", "v1.2.3-beta.9"], "v1.2.3-rc.1"],
      ["v1.2.3-rc.1", ["v1.2.3"], undefined],
      ["v1.2.3-rc.2", ["v1.2.3-rc.1", "v1.2.3-rc.10"], "v1.2.3-rc.1"],
      ["v1.2.3-alpha", ["v1.2.3-9"], "v1.2.3-9"],
      ["v1.2.3-9", ["v1.2.3-alpha"], undefined],
      ["v1.2.3-beta", ["v1.2.3-alpha"], "v1.2.3-alpha"],
      ["v1.2.3-alpha.1", ["v1.2.3-alpha"], "v1.2.3-alpha"],
      ["v1.2.3-alpha", ["v1.2.3-alpha.1"], undefined],
      ["v1.2.3-alpha.1", ["v1.2.3-alpha.1"], undefined],
      [
        "v1.2.3+build.2",
        ["v1.2.2+build.9", "broken", "v1.2.3-nightly.20260714.1"],
        "v1.2.2+build.9",
      ],
    ] as const;

    for (const [current, tags, expected] of cases) {
      assert.equal(yield* resolvePreviousReleaseTag("stable", current, tags), expected);
    }
  }),
);

it.effect("enforces strict SemVer syntax and ASCII prerelease precedence", () =>
  Effect.gen(function* () {
    for (const invalidTag of [
      "v01.2.3",
      "v1.02.3",
      "v1.2.03",
      "v1.2.3-01",
      "v1.2.3-alpha..1",
      "v1.2.3-alpha.",
      "v1.2.3-",
      "v1.2.3+build..1",
      "v1.2.3+",
      "v1.2.3-caf\u00e9",
    ]) {
      const error = yield* resolvePreviousReleaseTag("stable", invalidTag, []).pipe(Effect.flip);
      assert.equal(error._tag, "InvalidReleaseTagError");
    }

    assert.equal(
      yield* resolvePreviousReleaseTag("stable", "v1.0.0-z", ["v1.0.0-Z", "v1.0.0-a"]),
      "v1.0.0-a",
    );
    assert.equal(
      yield* resolvePreviousReleaseTag("stable", "v1.0.0-9007199254740993", [
        "v1.0.0-9007199254740991",
        "v1.0.0-9007199254740992",
      ]),
      "v1.0.0-9007199254740992",
    );
    assert.equal(
      yield* resolvePreviousReleaseTag("stable", "v2.0.0", [
        "v01.9.9",
        "v1.0.0-alpha..1",
        "v1.9.9+build.1",
      ]),
      "v1.9.9+build.1",
    );
    assert.isUndefined(
      yield* resolvePreviousReleaseTag("stable", "v1.2.3+build.2", ["v1.2.3+build.1"]),
    );
  }),
);

it.effect("orders nightly versions by core, date, and run number", () =>
  Effect.gen(function* () {
    const cases = [
      ["v2.0.0-nightly.20260714.2", ["v1.9.9-nightly.20260714.99"], "v1.9.9-nightly.20260714.99"],
      ["v1.2.0-nightly.20260714.2", ["v1.1.9-nightly.20260714.99"], "v1.1.9-nightly.20260714.99"],
      ["v1.2.3-nightly.20260714.2", ["v1.2.2-nightly.20260715.99"], "v1.2.2-nightly.20260715.99"],
      ["v1.2.3-nightly.20260714.2", ["v1.2.3-nightly.20260713.99"], "v1.2.3-nightly.20260713.99"],
      ["v1.2.3-nightly.20260714.2", ["v1.2.3-nightly.20260714.1"], "v1.2.3-nightly.20260714.1"],
      ["v1.2.3-nightly.20260714.2", ["invalid", "v1.2.3-nightly.20260714.2"], undefined],
    ] as const;
    for (const [current, tags, expected] of cases) {
      assert.equal(yield* resolvePreviousReleaseTag("nightly", current, tags), expected);
    }
  }),
);

it.effect("rejects malformed nightly SemVer components and calendar dates", () =>
  Effect.gen(function* () {
    for (const invalidTag of [
      "v01.2.3-nightly.20260714.1",
      "v1.02.3-nightly.20260714.1",
      "v1.2.03-nightly.20260714.1",
      "v1.2.3-nightly.20260229.1",
      "v1.2.3-nightly.20261301.1",
      "v1.2.3-nightly.20260714.01",
      "v1.2.3-nightly.20260714.0",
    ]) {
      const error = yield* resolvePreviousReleaseTag("nightly", invalidTag, []).pipe(Effect.flip);
      assert.equal(error._tag, "InvalidReleaseTagError");
    }

    assert.equal(
      yield* resolvePreviousReleaseTag("nightly", "v1.2.3-nightly.20260714.3", [
        "v1.2.3-nightly.20260229.9",
        "v1.2.3-nightly.20260714.01",
        "v1.2.3-nightly.20260714.2",
      ]),
      "v1.2.3-nightly.20260714.2",
    );
  }),
);

it.effect("preserves git tag spawn context and the exact platform cause", () => {
  const cause = PlatformError.systemError({
    _tag: "NotFound",
    module: "ChildProcess",
    method: "spawn",
    description: "git was not found",
  });

  return Effect.gen(function* () {
    const error = yield* listGitTags("/repo").pipe(
      Effect.scoped,
      Effect.provideService(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.fail(cause)),
      ),
      Effect.flip,
    );

    if (error._tag !== "ReleaseTagListProcessError") {
      return assert.fail(`Unexpected error: ${error._tag}`);
    }
    assert.equal(error.operation, "spawn");
    assert.equal(error.executable, "git");
    assert.equal(error.argumentCount, 2);
    assert.equal(error.cwd, "/repo");
    assert.strictEqual(error.cause, cause);
    assert.notProperty(error, "args");
    assert.notInclude(error.message, cause.message);
  });
});

it.effect("distinguishes stdout and stderr read failures", () =>
  Effect.gen(function* () {
    for (const [stream, operation] of [
      ["stdout", "read-stdout"],
      ["stderr", "read-stderr"],
    ] as const) {
      const cause = PlatformError.systemError({
        _tag: "Unknown",
        module: "ChildProcess",
        method: stream,
        description: `${stream} unavailable`,
      });
      const error = yield* listGitTags("/repo").pipe(
        Effect.scoped,
        Effect.provideService(
          ChildProcessSpawner.ChildProcessSpawner,
          ChildProcessSpawner.make(() =>
            Effect.succeed(
              mockHandle({
                exitCode: 0,
                ...(stream === "stdout" ? { stdoutError: cause } : { stderrError: cause }),
              }),
            ),
          ),
        ),
        Effect.flip,
      );

      if (error._tag !== "ReleaseTagListProcessError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, operation);
      assert.strictEqual(error.cause, cause);
    }
  }),
);

it.effect("reports git tag non-zero exits without manufacturing a cause", () =>
  Effect.gen(function* () {
    const error = yield* listGitTags("/repo").pipe(
      Effect.scoped,
      Effect.provideService(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() =>
          Effect.succeed(
            mockHandle({
              exitCode: 17,
              stdout: "v1.2.3\n",
              stderr: "fatal: repository unavailable\n",
            }),
          ),
        ),
      ),
      Effect.flip,
    );

    if (error._tag !== "ReleaseTagListProcessExitError") {
      return assert.fail(`Unexpected error: ${error._tag}`);
    }
    assert.equal(error.executable, "git");
    assert.equal(error.argumentCount, 2);
    assert.equal(error.cwd, "/repo");
    assert.equal(error.exitCode, 17);
    assert.equal(error.stdoutLength, 7);
    assert.equal(error.stderrLength, 30);
    assert.notProperty(error, "cause");
    assert.notProperty(error, "stdout");
    assert.notProperty(error, "stderr");
    assert.equal(error.message, "Release tag listing exited with code 17.");
  }),
);

it.effect("lists trimmed non-empty tags and maps exit-code stream failures", () =>
  Effect.gen(function* () {
    const tags = yield* listGitTags("/repo").pipe(
      Effect.scoped,
      Effect.provideService(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() =>
          Effect.succeed(mockHandle({ exitCode: 0, stdout: " v1.0.0\r\n\n v1.1.0 \n" })),
        ),
      ),
    );
    assert.deepStrictEqual(tags, ["v1.0.0", "v1.1.0"]);

    const cause = PlatformError.systemError({
      _tag: "Unknown",
      module: "ChildProcess",
      method: "exitCode",
    });
    const error = yield* listGitTags("/repo").pipe(
      Effect.scoped,
      Effect.provideService(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() =>
          Effect.succeed(mockHandle({ exitCode: 0, exitError: cause })),
        ),
      ),
      Effect.flip,
    );
    assert.equal(error._tag, "ReleaseTagListProcessError");
    if (error._tag !== "ReleaseTagListProcessError") {
      return assert.fail(`Unexpected error: ${error._tag}`);
    }
    assert.equal(error.operation, "wait-for-exit");
    assert.strictEqual(error.cause, cause);
  }),
);

it.effect("preserves the GITHUB_OUTPUT append path and exact cause", () => {
  const outputPath = "/tmp/previous-tag-github-output";
  const appendCause = PlatformError.systemError({
    _tag: "PermissionDenied",
    module: "FileSystem",
    method: "writeFileString",
    pathOrDescriptor: outputPath,
  });

  return Effect.gen(function* () {
    const appendError = yield* writePreviousReleaseTagOutput("v1.2.3", true).pipe(
      Effect.provideService(
        FileSystem.FileSystem,
        FileSystem.makeNoop({
          writeFileString: () => Effect.fail(appendCause),
        }),
      ),
      Effect.provideService(
        ConfigProvider.ConfigProvider,
        ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } }),
      ),
      Effect.flip,
    );

    if (appendError._tag !== "PreviousReleaseTagGitHubOutputAppendError") {
      return assert.fail(`Unexpected error: ${appendError._tag}`);
    }
    assert.equal(appendError.outputPath, outputPath);
    assert.strictEqual(appendError.cause, appendCause);
    assert.notProperty(appendError, "contents");
    assert.notInclude(appendError.message, appendCause.message);
  });
});

it.effect("writes stdout and successful GITHUB_OUTPUT entries without global mutation", () => {
  const stdout: Array<string> = [];
  return Effect.gen(function* () {
    yield* writePreviousReleaseTagOutput(undefined, false, (entry) => stdout.push(entry)).pipe(
      Effect.provideService(FileSystem.FileSystem, FileSystem.makeNoop({})),
    );
    assert.deepStrictEqual(stdout, ["previous_tag=\n"]);

    const outputPath = "/previous-output";
    const writes: Array<readonly [string, string, unknown]> = [];
    yield* writePreviousReleaseTagOutput("v1.2.3", true).pipe(
      Effect.provideService(
        FileSystem.FileSystem,
        FileSystem.makeNoop({
          writeFileString: (path, contents, options) => {
            writes.push([String(path), contents, options]);
            return Effect.void;
          },
        }),
      ),
      Effect.provide(
        ConfigProvider.layer(ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } })),
      ),
    );
    assert.deepStrictEqual(writes, [[outputPath, "previous_tag=v1.2.3\n", { flag: "a" }]]);
  });
});

it.effect("preserves a missing GITHUB_OUTPUT configuration error", () =>
  Effect.gen(function* () {
    const error = yield* writePreviousReleaseTagOutput("v1.2.3", true).pipe(
      Effect.provideService(FileSystem.FileSystem, FileSystem.makeNoop({})),
      Effect.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }))),
      Effect.flip,
    );
    assert.equal(error._tag, "PreviousReleaseTagGitHubOutputConfigError");
    assert.instanceOf(error.cause, Config.ConfigError);
    assert.equal(
      error.message,
      "Failed to resolve the GITHUB_OUTPUT path for the previous release tag.",
    );
  }),
);

it.layer(NodeServices.layer)("resolve-previous-release-tag CLI", (it) => {
  it.effect("runs tag listing, resolution, and GitHub output through the public command", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "previous-tag-cli-" });
      const outputPath = path.join(tempDir, "output.txt");
      const runCli = Command.runWith(resolvePreviousReleaseTagCommand, { version: "0.0.0" });
      yield* runCli(["--channel", "stable", "--current-tag", "v2.0.0", "--github-output"]).pipe(
        Effect.scoped,
        Effect.provide(
          Layer.merge(
            ConfigProvider.layer(ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } })),
            Layer.succeed(
              ChildProcessSpawner.ChildProcessSpawner,
              ChildProcessSpawner.make(() =>
                Effect.succeed(mockHandle({ exitCode: 0, stdout: "v1.9.0\nv2.0.0\n" })),
              ),
            ),
          ),
        ),
      );
      assert.equal(yield* fs.readFileString(outputPath), "previous_tag=v1.9.0\n");
    }),
  );
});

it("does not launch on import and launches once for direct execution", () => {
  const programs: Array<object> = [];
  const launch = <E, A>(program: Effect.Effect<A, E, never>) => programs.push(program);
  assert.isFalse(runResolvePreviousReleaseTagMain(false, launch));
  assert.isTrue(runResolvePreviousReleaseTagMain(true, launch));
  assert.lengthOf(programs, 1);
});
