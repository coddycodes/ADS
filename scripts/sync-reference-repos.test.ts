import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { Command } from "effect/cli";
import { ChildProcess, ChildProcessSpawner } from "effect/process";

import { referenceRepos } from "./lib/reference-repos.ts";
import type { ReferenceRepo } from "./lib/reference-repos.ts";
import {
  planReferenceRepoSync,
  ReferenceRepoGitSnapshotError,
  resolveReferenceRepoRef,
  runGitCommand,
  runSyncReferenceReposMain,
  isReferenceRepoSyncError,
  syncReferenceReposCommand,
  syncReferenceRepos,
  type ReferenceRepoGitCommandRunner,
  type ReferenceRepoSyncPhase,
  type ReferenceRepoSyncPlan,
} from "./sync-reference-repos.ts";

const encoder = new TextEncoder();
const effectSmol = referenceRepos[0]!;
const alchemyEffect = referenceRepos[1]!;
const REFERENCE_REPO_SYNC_LOCK_NAME = "bibcode-reference-repos-sync.lock";

const collectText = <E>(stream: Stream.Stream<Uint8Array, E>): Effect.Effect<string, E> =>
  stream.pipe(
    Stream.decodeText(),
    Stream.runFold(
      () => "",
      (acc, chunk) => acc + chunk,
    ),
  );

const runGit = Effect.fn("syncReferenceReposTest.runGit")(function* (
  cwd: string,
  args: ReadonlyArray<string>,
) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const child = yield* spawner.spawn(ChildProcess.make("git", args, { cwd }));
  const [stdout, stderr, exitCode] = yield* Effect.all(
    [collectText(child.stdout), collectText(child.stderr), child.exitCode.pipe(Effect.map(Number))],
    { concurrency: "unbounded" },
  );
  if (exitCode !== 0) {
    return yield* Effect.die(new Error(`git test command failed (${exitCode}): ${stderr}`));
  }
  return stdout;
});

const runGitWithPathspecGlobbing: ReferenceRepoGitCommandRunner = (
  rootDir,
  _plan,
  _phase,
  args,
  options,
) =>
  Effect.gen(function* () {
    const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
    const child = yield* spawner
      .spawn(
        ChildProcess.make("git", args, {
          cwd: rootDir,
          env: {
            GIT_GLOB_PATHSPECS: "1",
            ...(options?.indexFile === undefined ? {} : { GIT_INDEX_FILE: options.indexFile }),
          },
          extendEnv: true,
        }),
      )
      .pipe(Effect.orDie);
    const [stdout, stderr, exitCode] = yield* Effect.all(
      [
        collectText(child.stdout),
        collectText(child.stderr),
        child.exitCode.pipe(Effect.map(Number)),
      ],
      { concurrency: "unbounded" },
    ).pipe(Effect.orDie);
    if (exitCode !== 0) {
      return yield* Effect.die(new Error(`git glob-pathspec test command failed (${exitCode})`));
    }
    return { stderr, stdout };
  });

const writeFixtureFile = Effect.fn("syncReferenceReposTest.writeFixtureFile")(function* (
  root: string,
  relativePath: string,
  contents: string,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const filePath = path.join(root, relativePath);
  yield* fs.makeDirectory(path.dirname(filePath), { recursive: true });
  yield* fs.writeFileString(filePath, contents);
});

const initializeRepository = Effect.fn("syncReferenceReposTest.initializeRepository")(function* (
  root: string,
) {
  const fs = yield* FileSystem.FileSystem;
  yield* fs.makeDirectory(root, { recursive: true });
  yield* runGit(root, ["init", "--quiet"]);
  yield* runGit(root, ["config", "user.name", "Reference Sync Test"]);
  yield* runGit(root, ["config", "user.email", "reference-sync-test@invalid.local"]);
});

const getReferenceRepoSyncLockPath = Effect.fn(
  "syncReferenceReposTest.getReferenceRepoSyncLockPath",
)(function* (root: string) {
  const path = yield* Path.Path;
  const commonDir = (yield* runGit(root, [
    "rev-parse",
    "--path-format=absolute",
    "--git-common-dir",
  ])).trim();
  return path.join(commonDir, REFERENCE_REPO_SYNC_LOCK_NAME);
});

const commitAll = Effect.fn("syncReferenceReposTest.commitAll")(function* (
  root: string,
  message: string,
) {
  yield* runGit(root, ["add", "-A"]);
  yield* runGit(root, ["commit", "--quiet", "-m", message]);
});

const makeSnapshotFixture = Effect.fn("syncReferenceReposTest.makeSnapshotFixture")(function* (
  root: string,
) {
  const path = yield* Path.Path;
  const upstream = path.join(root, "upstream");
  const target = path.join(root, "target");

  yield* initializeRepository(upstream);
  yield* writeFixtureFile(upstream, "SQL/query.ts", "export const query = true;\n");
  yield* writeFixtureFile(upstream, "bin/run.sh", "#!/bin/sh\nexit 0\n");
  yield* writeFixtureFile(upstream, "ignored.fixture", "tracked upstream fixture\n");
  yield* writeFixtureFile(upstream, ".vendor/nested-gitlink.txt", "prune me\n");
  yield* writeFixtureFile(upstream, "kept.txt", "upstream\n");
  yield* runGit(upstream, ["add", "-A"]);
  yield* runGit(upstream, ["update-index", "--chmod=+x", "--", "bin/run.sh"]);
  yield* runGit(upstream, ["commit", "--quiet", "-m", "upstream snapshot"]);
  yield* runGit(upstream, ["tag", "v1"]);

  yield* initializeRepository(target);
  yield* writeFixtureFile(target, ".gitignore", "*.fixture\n");
  yield* writeFixtureFile(target, "version.json", '{"version":"v1"}\n');
  yield* writeFixtureFile(target, ".repos/sample/Sql/old.ts", "old casing\n");
  yield* writeFixtureFile(target, ".repos/sample/deleted.txt", "delete me\n");
  yield* writeFixtureFile(target, "unrelated.txt", "preserve me\n");
  yield* commitAll(target, "target baseline");

  return {
    repo: {
      id: "sample",
      prefix: ".repos/sample",
      repository: upstream,
      latestRef: "v1",
      versionSourcePath: "version.json",
      packageVersionPath: ["version"],
      versionTagPrefix: "",
      prunePaths: [".vendor"],
    } satisfies ReferenceRepo,
    target,
    upstream,
  };
});

function simulatedSnapshotError(
  rootDir: string,
  plan: ReferenceRepoSyncPlan,
  phase: ReferenceRepoSyncPhase,
): ReferenceRepoGitSnapshotError {
  return new ReferenceRepoGitSnapshotError({
    operation: "exit",
    phase,
    repoId: plan.repo.id,
    action: "replace",
    repository: plan.repo.repository,
    ref: plan.ref,
    rootDir,
    argumentCount: 0,
    exitCode: 91,
    stdoutLength: 0,
    stderrLength: 0,
  });
}

function mockHandle(
  options: {
    readonly exitCode?: number;
    readonly stdout?: string;
    readonly stderr?: string;
    readonly stdoutError?: PlatformError.PlatformError;
    readonly stderrError?: PlatformError.PlatformError;
    readonly exitError?: PlatformError.PlatformError;
  } = {},
) {
  return ChildProcessSpawner.makeHandle({
    pid: ChildProcessSpawner.ProcessId(1),
    exitCode: options.exitError
      ? Effect.fail(options.exitError)
      : Effect.succeed(ChildProcessSpawner.ExitCode(options.exitCode ?? 0)),
    isRunning: Effect.succeed(false),
    kill: () => Effect.void,
    unref: Effect.succeed(Effect.void),
    stdin: Sink.drain,
    stdout: options.stdoutError
      ? Stream.fail(options.stdoutError)
      : Stream.make(encoder.encode(options.stdout ?? "done\n")),
    stderr: options.stderrError
      ? Stream.fail(options.stderrError)
      : Stream.make(encoder.encode(options.stderr ?? "")),
    all: Stream.empty,
    getInputFd: () => Sink.drain,
    getOutputFd: () => Stream.empty,
  });
}

function mockSpawnerLayer(
  commands: Array<{ readonly command: string; readonly args: ReadonlyArray<string> }>,
  handle = mockHandle(),
) {
  return Layer.succeed(
    ChildProcessSpawner.ChildProcessSpawner,
    ChildProcessSpawner.make((command) => {
      const childProcess = command as unknown as {
        readonly command: string;
        readonly args: ReadonlyArray<string>;
      };
      commands.push({
        command: childProcess.command,
        args: childProcess.args,
      });
      return Effect.succeed(handle);
    }),
  );
}

it.layer(NodeServices.layer)("sync-reference-repos", (it) => {
  it.effect("resolves the effect-smol tag from the root catalog", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-version-",
      });
      yield* fs.writeFileString(
        path.join(rootDir, "pnpm-workspace.yaml"),
        "catalog:\n  effect: 4.0.0-beta.73\n",
      );

      assert.equal(
        yield* resolveReferenceRepoRef(effectSmol, rootDir, false),
        "effect@4.0.0-beta.73",
      );
    }),
  );

  it.effect("uses the latest branch without reading package versions", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-latest-",
      });

      assert.equal(yield* resolveReferenceRepoRef(effectSmol, rootDir, true), "main");
    }),
  );

  it.effect("preserves version source read context and the filesystem cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-read-error-",
      });
      const sourcePath = path.join(rootDir, effectSmol.versionSourcePath);

      const error = yield* resolveReferenceRepoRef(effectSmol, rootDir, false).pipe(Effect.flip);

      if (error._tag !== "ReferenceRepoVersionSourceError") {
        assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "read");
      assert.equal(error.repoId, effectSmol.id);
      assert.equal(error.sourcePath, sourcePath);
      assert.ok(error.cause !== undefined);
      assert.ok(!error.message.includes(String((error.cause as Error).message)));
    }),
  );

  it.effect("preserves version source parse context and the schema cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-parse-error-",
      });
      const sourcePath = path.join(rootDir, alchemyEffect.versionSourcePath);
      yield* fs.makeDirectory(path.dirname(sourcePath), { recursive: true });
      yield* fs.writeFileString(sourcePath, "{");

      const error = yield* resolveReferenceRepoRef(alchemyEffect, rootDir, false).pipe(Effect.flip);

      if (error._tag !== "ReferenceRepoVersionSourceError") {
        assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "parse");
      assert.equal(error.repoId, alchemyEffect.id);
      assert.equal(error.sourcePath, sourcePath);
      assert.ok(error.cause !== undefined);
      assert.ok(!error.message.includes(String((error.cause as Error).message)));
    }),
  );

  it.effect("reports the unresolved package path without inventing a cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-resolution-error-",
      });
      const sourcePath = path.join(rootDir, alchemyEffect.versionSourcePath);
      yield* fs.makeDirectory(path.dirname(sourcePath), { recursive: true });
      yield* fs.writeFileString(sourcePath, '{"dependencies":{}}');

      const error = yield* resolveReferenceRepoRef(alchemyEffect, rootDir, false).pipe(Effect.flip);

      if (error._tag !== "ReferenceRepoVersionResolutionError") {
        assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.repoId, alchemyEffect.id);
      assert.equal(error.sourcePath, sourcePath);
      assert.deepStrictEqual(error.packageVersionPath, ["dependencies", "alchemy"]);
      assert.ok(!("cause" in error));
      assert.equal(
        error.message,
        `No version was found for reference repo "${alchemyEffect.id}" at ${sourcePath}:dependencies.alchemy.`,
      );
      assert.isTrue(isReferenceRepoSyncError(error));
    }),
  );

  it.effect("resolves the alchemy-effect tag from the relay package", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-alchemy-version-",
      });
      yield* fs.makeDirectory(path.join(rootDir, "infra", "relay"), { recursive: true });
      yield* fs.writeFileString(
        path.join(rootDir, "infra", "relay", "package.json"),
        '{"dependencies":{"alchemy":"2.0.0-beta.49"}}',
      );

      assert.equal(yield* resolveReferenceRepoRef(alchemyEffect, rootDir, false), "v2.0.0-beta.49");
    }),
  );

  it.effect("resolves the alchemy-effect commit from a pkg.ing source", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-alchemy-pkg-ing-",
      });
      yield* fs.makeDirectory(path.join(rootDir, "infra", "relay"), { recursive: true });
      yield* fs.writeFileString(
        path.join(rootDir, "infra", "relay", "package.json"),
        '{"dependencies":{"alchemy":"https://pkg.ing/alchemy/cde008ab6b77783d3edbf5dc82750fbdfd279347"}}',
      );

      assert.equal(
        yield* resolveReferenceRepoRef(alchemyEffect, rootDir, false),
        "cde008ab6b77783d3edbf5dc82750fbdfd279347",
      );
    }),
  );

  it.effect("plans one history-independent snapshot replacement whether the prefix exists", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-plan-",
      });
      yield* fs.writeFileString(
        path.join(rootDir, "pnpm-workspace.yaml"),
        "catalog:\n  effect: 4.0.0-beta.73\n",
      );

      const missingPrefixPlan = yield* planReferenceRepoSync(effectSmol, rootDir, false);
      assert.equal(missingPrefixPlan.action, "replace");
      assert.deepStrictEqual(missingPrefixPlan.fetchArgs, [
        "fetch",
        "--no-tags",
        "https://github.com/Effect-TS/effect.git",
        "effect@4.0.0-beta.73",
      ]);

      yield* fs.makeDirectory(path.join(rootDir, effectSmol.prefix), { recursive: true });
      assert.deepStrictEqual(
        yield* planReferenceRepoSync(effectSmol, rootDir, false),
        missingPrefixPlan,
      );
    }),
  );

  it.effect("rejects unsafe prefixes and prune paths before filesystem planning", () =>
    Effect.gen(function* () {
      const invalidPrefixes = [
        "",
        ".repos",
        ".repos/",
        ".repos//escape",
        ".repos/./escape",
        ".repos/../escape",
        "/absolute",
        "C:/absolute",
        "//server/share",
        ".repos\\escape",
        ".repos/CON",
        ".repos/prn.txt",
        ".repos/AuX.log",
        ".repos/NUL",
        ".repos/com1.json",
        ".repos/COM9",
        ".repos/lpt1.cache",
        ".repos/LPT9",
        ".repos/trailing.",
        ".repos/trailing ",
        ".repos/data:stream",
        ".repos/control\u0001name",
        ".repos/delete\u007fname",
        ".repos/less<than",
        ".repos/greater>than",
        '.repos/double"quote',
        ".repos/pipe|name",
        ".repos/question?mark",
        ".repos/star*name",
      ];
      const invalidPrunePaths = [
        "",
        ".",
        "..",
        "nested/../escape",
        "nested//escape",
        "nested/./escape",
        "nested/",
        "/absolute",
        "C:/absolute",
        "//server/share",
        "nested\\escape",
        ".repos/another-subtree",
        "CON",
        "nested/prn.txt",
        "AuX.log",
        "NUL",
        "com1.json",
        "COM9",
        "lpt1.cache",
        "LPT9",
        "nested/trailing.",
        "nested/trailing ",
        "nested/data:stream",
        "nested/control\u0001name",
        "nested/delete\u007fname",
        "nested/less<than",
        "nested/greater>than",
        'nested/double"quote',
        "nested/pipe|name",
        "nested/question?mark",
        "nested/star*name",
      ];
      let existsCalled = false;
      const rejectingFileSystem = FileSystem.makeNoop({
        exists: () => {
          existsCalled = true;
          return Effect.succeed(false);
        },
      });

      for (const prefix of invalidPrefixes) {
        const error = yield* planReferenceRepoSync({ ...effectSmol, prefix }, "/repo", true).pipe(
          Effect.provideService(FileSystem.FileSystem, rejectingFileSystem),
          Effect.flip,
        );
        if (error._tag !== "ReferenceRepoPathValidationError") {
          assert.fail(`Expected ReferenceRepoPathValidationError, got ${error._tag}`);
        }
        assert.equal(error.field, "prefix");
        assert.equal(error.value, prefix);
        assert.include(error.message, `unsafe prefix path "${prefix}"`);
      }

      for (const prunePath of invalidPrunePaths) {
        const error = yield* planReferenceRepoSync(
          { ...effectSmol, prunePaths: [prunePath] },
          "/repo",
          true,
        ).pipe(Effect.provideService(FileSystem.FileSystem, rejectingFileSystem), Effect.flip);
        if (error._tag !== "ReferenceRepoPathValidationError") {
          assert.fail(`Expected ReferenceRepoPathValidationError, got ${error._tag}`);
        }
        assert.equal(error.field, "prunePath");
        assert.equal(error.value, prunePath);
      }

      assert.isFalse(existsCalled);
    }),
  );

  it.effect("accepts normalized dotted and hyphenated subtree paths", () =>
    Effect.gen(function* () {
      const plan = yield* planReferenceRepoSync(
        {
          ...effectSmol,
          prefix: ".repos/effect-smol.v2",
          prunePaths: ["docs.v2/read-me", "packages/effect-core"],
        },
        "/repo",
        true,
      ).pipe(
        Effect.provideService(
          FileSystem.FileSystem,
          FileSystem.makeNoop({ exists: () => Effect.succeed(false) }),
        ),
      );

      assert.deepStrictEqual(plan.fetchArgs, [
        "fetch",
        "--no-tags",
        effectSmol.repository,
        effectSmol.latestRef,
      ]);
    }),
  );

  it.effect("replaces an existing prefix exactly and stages only the fetched snapshot", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-replace-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const headBefore = (yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim();

      const plans = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
      );

      assert.deepStrictEqual(
        plans.map(({ action }) => action),
        ["replace"],
      );
      assert.equal((yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim(), headBefore);
      assert.sameMembers(yield* fs.readDirectory(path.join(fixture.target, ".repos/sample")), [
        "SQL",
        "bin",
        "ignored.fixture",
        "kept.txt",
      ]);
      assert.deepStrictEqual(
        yield* fs.readDirectory(path.join(fixture.target, ".repos/sample/SQL")),
        ["query.ts"],
      );
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/.vendor")));
      assert.equal(
        yield* fs.readFileString(path.join(fixture.target, "unrelated.txt")),
        "preserve me\n",
      );

      const stagedPaths = (yield* runGit(fixture.target, ["diff", "--cached", "--name-only"]))
        .trim()
        .split("\n");
      assert.isAbove(stagedPaths.length, 0);
      assert.isTrue(stagedPaths.every((path) => path.startsWith(".repos/sample/")));
      assert.include(stagedPaths, ".repos/sample/SQL/query.ts");
      assert.include(stagedPaths, ".repos/sample/ignored.fixture");
      assert.include(stagedPaths, ".repos/sample/Sql/old.ts");
      assert.match(
        yield* runGit(fixture.target, ["ls-files", "--stage", ".repos/sample/bin/run.sh"]),
        /^100755 /u,
      );
      assert.match(
        yield* runGit(fixture.target, ["ls-files", "--stage", ".repos/sample/ignored.fixture"]),
        /^100644 /u,
      );

      for (const line of (yield* runGit(fixture.target, ["status", "--porcelain=v1"]))
        .trim()
        .split("\n")) {
        assert.notEqual(line[0], " ");
        assert.equal(line[1], " ");
      }
    }),
  );

  it.effect("leaves the target and index unchanged when fetch fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-fetch-failure-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const headBefore = (yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim();
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let fetchObservedLock = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "fetch") {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        return fs.exists(lockPath).pipe(
          Effect.orDie,
          Effect.tap((exists) =>
            Effect.sync(() => {
              fetchObservedLock = exists;
            }),
          ),
          Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id, latest: true },
        [{ ...fixture.repo, latestRef: "missing-ref" }],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoGitSnapshotError");
      if (error._tag !== "ReferenceRepoGitSnapshotError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.phase, "fetch");
      assert.isTrue(fetchObservedLock);
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal((yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim(), headBefore);
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect("leaves an unborn clean repository unchanged when snapshot construction fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-build-failure-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const unbornTarget = path.join(rootDir, "unborn-target");
      yield* initializeRepository(unbornTarget);

      const error = yield* syncReferenceRepos(
        { rootDir: unbornTarget, repoId: fixture.repo.id, latest: true },
        [fixture.repo],
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoGitSnapshotError");
      if (error._tag !== "ReferenceRepoGitSnapshotError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.phase, "build");
      assert.equal(yield* runGit(unbornTarget, ["status", "--porcelain=v1"]), "");
      assert.equal((yield* runGit(unbornTarget, ["rev-list", "--count", "--all"])).trim(), "0");
      assert.isFalse(yield* fs.exists(path.join(unbornTarget, ".repos/sample")));
    }),
  );

  it.effect("rejects a dirty repository before fetch or snapshot mutation", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-dirty-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      yield* writeFixtureFile(fixture.target, "dirty.txt", "uncommitted\n");
      const headBefore = (yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim();
      const statusBefore = yield* runGit(fixture.target, ["status", "--porcelain=v1"]);
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let cleanCheckObservedLock = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "verify-clean" || args[0] !== "status") {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        return fs.exists(lockPath).pipe(
          Effect.orDie,
          Effect.tap((exists) =>
            Effect.sync(() => {
              cleanCheckObservedLock = exists;
            }),
          ),
          Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoWorkspaceDirtyError");
      assert.isTrue(cleanCheckObservedLock);
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal((yield* runGit(fixture.target, ["rev-parse", "HEAD"])).trim(), headBefore);
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), statusBefore);
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect(
    "rejects a pre-existing ignored artifact inside the managed prefix without deleting it",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-ignored-dirty-" });
        const fixture = yield* makeSnapshotFixture(rootDir);
        const ignoredPath = path.join(fixture.target, ".repos/sample/pre-existing.fixture");
        yield* fs.writeFileString(ignoredPath, "preserve me\n");
        const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
        let fetched = false;
        const commandRunner: ReferenceRepoGitCommandRunner = (
          commandRoot,
          plan,
          phase,
          args,
          options,
        ) => {
          if (phase === "fetch") {
            fetched = true;
          }
          return runGitCommand(commandRoot, plan, phase, args, options);
        };

        const error = yield* syncReferenceRepos(
          { rootDir: fixture.target, repoId: fixture.repo.id },
          [fixture.repo],
          commandRunner,
        ).pipe(Effect.flip);

        assert.equal(error._tag, "ReferenceRepoWorkspaceDirtyError");
        assert.isFalse(fetched);
        assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
        assert.equal(yield* fs.readFileString(ignoredPath), "preserve me\n");
        assert.match(
          yield* runGit(fixture.target, [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            fixture.repo.prefix,
          ]),
          /pre-existing\.fixture/u,
        );
      }),
  );

  it.effect("treats a bracketed managed prefix literally during successful replacement", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-literal-prefix-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const repo = { ...fixture.repo, prefix: ".repos/[ab]" };
      const trackedA = path.join(fixture.target, ".repos/a");
      const trackedB = path.join(fixture.target, ".repos/b");
      yield* writeFixtureFile(fixture.target, ".repos/a", "tracked a\n");
      yield* writeFixtureFile(fixture.target, ".repos/b", "tracked b\n");
      yield* commitAll(fixture.target, "tracked pathspec lookalikes");

      yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: repo.id },
        [repo],
        runGitWithPathspecGlobbing,
      );

      assert.equal(yield* fs.readFileString(trackedA), "tracked a\n");
      assert.equal(yield* fs.readFileString(trackedB), "tracked b\n");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
      assert.deepStrictEqual(
        (yield* runGit(fixture.target, ["diff", "--cached", "--name-only"])).trim().split("\n"),
        [
          ".repos/[ab]/SQL/query.ts",
          ".repos/[ab]/bin/run.sh",
          ".repos/[ab]/ignored.fixture",
          ".repos/[ab]/kept.txt",
        ],
      );
    }),
  );

  it.effect("preserves ignored bracket-path lookalikes during successful replacement", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-literal-ignored-success-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const repo = { ...fixture.repo, prefix: ".repos/[ab]" };
      const ignoredA = path.join(fixture.target, ".repos/a");
      const ignoredB = path.join(fixture.target, ".repos/b");
      yield* fs.writeFileString(
        path.join(fixture.target, ".gitignore"),
        "*.fixture\n.repos/a\n.repos/b\n",
      );
      yield* commitAll(fixture.target, "ignore pathspec lookalikes");
      yield* fs.writeFileString(ignoredA, "ignored a\n");
      yield* fs.writeFileString(ignoredB, "ignored b\n");

      yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: repo.id },
        [repo],
        runGitWithPathspecGlobbing,
      );

      assert.equal(yield* fs.readFileString(ignoredA), "ignored a\n");
      assert.equal(yield* fs.readFileString(ignoredB), "ignored b\n");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
    }),
  );

  it.effect("treats a bracketed prune path literally", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-literal-prune-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      yield* writeFixtureFile(fixture.upstream, ".vendor/a", "keep a\n");
      yield* writeFixtureFile(fixture.upstream, ".vendor/b", "keep b\n");
      yield* writeFixtureFile(fixture.upstream, ".vendor/[ab]", "prune exact\n");
      yield* commitAll(fixture.upstream, "bracketed prune fixture");
      yield* runGit(fixture.upstream, ["tag", "--force", "v1"]);
      const repo = { ...fixture.repo, prunePaths: [".vendor/[ab]"] };

      yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: repo.id },
        [repo],
        runGitWithPathspecGlobbing,
      );

      assert.equal(
        yield* fs.readFileString(path.join(fixture.target, repo.prefix, ".vendor/a")),
        "keep a\n",
      );
      assert.equal(
        yield* fs.readFileString(path.join(fixture.target, repo.prefix, ".vendor/b")),
        "keep b\n",
      );
      assert.isFalse(yield* fs.exists(path.join(fixture.target, repo.prefix, ".vendor/[ab]")));
    }),
  );

  it.effect("treats a bracketed managed prefix literally during apply rollback", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-literal-rollback-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const repo = { ...fixture.repo, prefix: ".repos/[ab]" };
      const trackedA = path.join(fixture.target, ".repos/a");
      const trackedB = path.join(fixture.target, ".repos/b");
      yield* writeFixtureFile(fixture.target, ".repos/a", "tracked a\n");
      yield* writeFixtureFile(fixture.target, ".repos/b", "tracked b\n");
      yield* commitAll(fixture.target, "tracked pathspec lookalikes");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      let failedApply = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "apply" || failedApply) {
          return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options).pipe(
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: repo.id },
        [repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoApplyRolledBackError");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* fs.readFileString(trackedA), "tracked a\n");
      assert.equal(yield* fs.readFileString(trackedB), "tracked b\n");
      assert.isFalse(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
    }),
  );

  it.effect("preserves ignored bracket-path lookalikes during apply rollback", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-literal-ignored-rollback-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const repo = { ...fixture.repo, prefix: ".repos/[ab]" };
      const ignoredA = path.join(fixture.target, ".repos/a");
      const ignoredB = path.join(fixture.target, ".repos/b");
      yield* fs.writeFileString(
        path.join(fixture.target, ".gitignore"),
        "*.fixture\n.repos/a\n.repos/b\n",
      );
      yield* commitAll(fixture.target, "ignore pathspec lookalikes");
      yield* fs.writeFileString(ignoredA, "ignored a\n");
      yield* fs.writeFileString(ignoredB, "ignored b\n");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      let failedApply = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "apply" || failedApply) {
          return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options).pipe(
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: repo.id },
        [repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoApplyRolledBackError");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* fs.readFileString(ignoredA), "ignored a\n");
      assert.equal(yield* fs.readFileString(ignoredB), "ignored b\n");
      assert.isFalse(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
    }),
  );

  it.effect("does not reject an ignored artifact outside the managed prefix", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-ignored-outside-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredPath = path.join(fixture.target, "normal-build.fixture");
      yield* fs.writeFileString(ignoredPath, "ordinary ignored output\n");

      yield* syncReferenceRepos({ rootDir: fixture.target, repoId: fixture.repo.id }, [
        fixture.repo,
      ]);

      assert.equal(yield* fs.readFileString(ignoredPath), "ordinary ignored output\n");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect("skips apply when the second cleanliness check finds an ignored managed artifact", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-ignored-clean-race-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredPath = path.join(fixture.target, ".repos/sample/raced.fixture");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      let buildTreeWrites = 0;
      let applyCalled = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "apply") {
          applyCalled = true;
        }
        const command = runGitCommand(commandRoot, plan, phase, args, options);
        if (phase !== "build" || args[0] !== "write-tree") {
          return command;
        }
        buildTreeWrites += 1;
        return buildTreeWrites === 2
          ? command.pipe(
              Effect.tap(() => fs.writeFileString(ignoredPath, "raced\n").pipe(Effect.orDie)),
            )
          : command;
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoWorkspaceDirtyError");
      assert.isFalse(applyCalled);
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* fs.readFileString(ignoredPath), "raced\n");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect("restores the original clean index and working tree after apply failure", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-apply-rollback-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let failedApply = false;
      let rollbackObservedLock = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "rollback") {
          return fs.exists(lockPath).pipe(
            Effect.orDie,
            Effect.tap((exists) =>
              Effect.sync(() => {
                rollbackObservedLock = rollbackObservedLock || exists;
              }),
            ),
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
          );
        }
        if (phase !== "apply" || failedApply) {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return runGitCommand(commandRoot, plan, phase, args, options).pipe(
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoApplyRolledBackError");
      assert.isTrue(rollbackObservedLock);
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect("removes an ignored untracked artifact left by a partial apply failure", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-partial-apply-rollback-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredPath = path.join(fixture.target, ".repos/sample/partial.fixture");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      let failedApply = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "apply" || failedApply) {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return fs.writeFileString(ignoredPath, "partial apply output\n").pipe(
          Effect.orDie,
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.isFalse(yield* fs.exists(ignoredPath));
      assert.equal(error._tag, "ReferenceRepoApplyRolledBackError");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
      assert.equal(
        yield* runGit(fixture.target, [
          "status",
          "--porcelain=v1",
          "--untracked-files=all",
          "--ignored=matching",
          "--",
          fixture.repo.prefix,
        ]),
        "",
      );
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect(
    "restores tracked and ignored state before propagating an apply interruption and releasing the lock",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const rootDir = yield* fs.makeTempDirectoryScoped({
          prefix: "sync-real-interrupted-apply-rollback-",
        });
        const fixture = yield* makeSnapshotFixture(rootDir);
        const repo = { ...fixture.repo, prefix: ".repos/[ab]" };
        const trackedLookalikeA = path.join(fixture.target, ".repos/a/tracked.txt");
        const trackedLookalikeB = path.join(fixture.target, ".repos/b/tracked.txt");
        const ignoredLookalikeA = path.join(fixture.target, ".repos/a/ignored.fixture");
        const ignoredLookalikeB = path.join(fixture.target, ".repos/b/ignored.fixture");
        const ignoredResidue = path.join(fixture.target, repo.prefix, "partial.fixture");
        yield* writeFixtureFile(fixture.target, ".repos/a/tracked.txt", "tracked a\n");
        yield* writeFixtureFile(fixture.target, ".repos/b/tracked.txt", "tracked b\n");
        yield* commitAll(fixture.target, "tracked pathspec lookalikes");
        yield* fs.writeFileString(ignoredLookalikeA, "ignored a\n");
        yield* fs.writeFileString(ignoredLookalikeB, "ignored b\n");

        const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
        const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
        const applyMutated = yield* Deferred.make<void>();
        let rollbackCommandCount = 0;
        let everyRollbackCommandObservedLock = true;
        const commandRunner: ReferenceRepoGitCommandRunner = (
          commandRoot,
          plan,
          phase,
          args,
          options,
        ) => {
          if (phase === "rollback") {
            rollbackCommandCount += 1;
            return fs.exists(lockPath).pipe(
              Effect.orDie,
              Effect.tap((exists) =>
                Effect.sync(() => {
                  everyRollbackCommandObservedLock = everyRollbackCommandObservedLock && exists;
                }),
              ),
              Effect.andThen(runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options)),
            );
          }
          if (phase !== "apply") {
            return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options);
          }
          return runGitWithPathspecGlobbing(commandRoot, plan, phase, args, options).pipe(
            Effect.andThen(fs.writeFileString(ignoredResidue, "partial apply output\n")),
            Effect.orDie,
            Effect.tap(() => Deferred.succeed(applyMutated, undefined)),
            Effect.andThen(Effect.never),
          );
        };

        const sync = yield* syncReferenceRepos(
          { rootDir: fixture.target, repoId: repo.id },
          [repo],
          commandRunner,
        ).pipe(Effect.forkChild({ startImmediately: true }));

        yield* Deferred.await(applyMutated);
        assert.isTrue(yield* fs.exists(lockPath));
        assert.notEqual((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
        assert.isTrue(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
        assert.isTrue(yield* fs.exists(ignoredResidue));

        yield* Fiber.interrupt(sync);
        const interruptedExit = yield* Fiber.await(sync);

        assert.isTrue(Exit.hasInterrupts(interruptedExit));
        assert.equal(rollbackCommandCount, 5);
        assert.isTrue(everyRollbackCommandObservedLock);
        assert.isFalse(yield* fs.exists(lockPath));
        assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
        assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
        assert.equal(
          yield* runGit(fixture.target, [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            repo.prefix,
          ]),
          "",
        );
        assert.isFalse(yield* fs.exists(ignoredResidue));
        assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
        assert.isFalse(yield* fs.exists(path.join(fixture.target, repo.prefix, "kept.txt")));
        assert.equal(yield* fs.readFileString(trackedLookalikeA), "tracked a\n");
        assert.equal(yield* fs.readFileString(trackedLookalikeB), "tracked b\n");
        assert.equal(yield* fs.readFileString(ignoredLookalikeA), "ignored a\n");
        assert.equal(yield* fs.readFileString(ignoredLookalikeB), "ignored b\n");
      }),
  );

  it.effect("quiesces apply and timed rollback resources before entering the next phase", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-command-scope-lifecycle-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredResidue = path.join(fixture.target, ".repos/sample/partial.fixture");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      const applyMutated = yield* Deferred.make<void>();
      const applyFinalizerStarted = yield* Deferred.make<void>();
      const allowApplyFinalizer = yield* Deferred.make<void>();
      const rollbackRestoreEntered = yield* Deferred.make<void>();
      const rollbackRestoreCompleted = yield* Deferred.make<void>();
      const rollbackFinalizerStarted = yield* Deferred.make<void>();
      const allowRollbackFinalizer = yield* Deferred.make<void>();
      const rollbackCleanupEntered = yield* Deferred.make<void>();
      const events: Array<string> = [];

      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "apply") {
          return Effect.acquireRelease(Effect.void, () =>
            Effect.gen(function* () {
              events.push("apply-finalizer-started");
              yield* Deferred.succeed(applyFinalizerStarted, undefined);
              yield* Deferred.await(allowApplyFinalizer);
              events.push("apply-finalizer-completed");
            }),
          ).pipe(
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
            Effect.andThen(fs.writeFileString(ignoredResidue, "partial apply output\n")),
            Effect.orDie,
            Effect.tap(() => Deferred.succeed(applyMutated, undefined)),
            Effect.andThen(Effect.never),
          );
        }
        if (phase === "rollback" && args[0] === "restore") {
          return Effect.acquireRelease(Effect.void, () =>
            Effect.gen(function* () {
              events.push("rollback-finalizer-started");
              yield* Deferred.succeed(rollbackFinalizerStarted, undefined);
              yield* Deferred.await(allowRollbackFinalizer);
              events.push("rollback-finalizer-completed");
            }),
          ).pipe(
            Effect.andThen(
              Effect.sync(() => {
                events.push("rollback-restore-entered");
              }),
            ),
            Effect.andThen(Deferred.succeed(rollbackRestoreEntered, undefined)),
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
            Effect.tap(() => Deferred.succeed(rollbackRestoreCompleted, undefined)),
          );
        }
        if (phase === "rollback" && args[0] === "clean") {
          return Effect.sync(() => {
            events.push("rollback-cleanup-entered");
          }).pipe(
            Effect.andThen(Deferred.succeed(rollbackCleanupEntered, undefined)),
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
          );
        }
        return runGitCommand(commandRoot, plan, phase, args, options);
      };

      const sync = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(applyMutated);
      assert.isTrue(yield* fs.exists(lockPath));
      assert.notEqual((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.isTrue(yield* fs.exists(ignoredResidue));

      const interruptRequest = yield* Fiber.interrupt(sync).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      const firstAfterInterrupt = yield* Effect.race(
        Deferred.await(applyFinalizerStarted).pipe(Effect.as("apply-finalizer-started" as const)),
        Deferred.await(rollbackRestoreEntered).pipe(Effect.as("rollback-restore-entered" as const)),
      );
      yield* Deferred.succeed(allowApplyFinalizer, undefined);
      yield* Deferred.await(rollbackRestoreCompleted);
      const firstAfterRestore = yield* Effect.race(
        Deferred.await(rollbackFinalizerStarted).pipe(
          Effect.as("rollback-finalizer-started" as const),
        ),
        Deferred.await(rollbackCleanupEntered).pipe(Effect.as("rollback-cleanup-entered" as const)),
      );
      yield* Deferred.succeed(allowRollbackFinalizer, undefined);
      yield* Fiber.join(interruptRequest);
      const interruptedExit = yield* Fiber.await(sync);

      assert.deepStrictEqual(
        [firstAfterInterrupt, firstAfterRestore],
        ["apply-finalizer-started", "rollback-finalizer-started"],
      );
      assert.isBelow(
        events.indexOf("apply-finalizer-completed"),
        events.indexOf("rollback-restore-entered"),
      );
      assert.isBelow(
        events.indexOf("rollback-finalizer-completed"),
        events.indexOf("rollback-cleanup-entered"),
      );
      assert.isTrue(Exit.hasInterrupts(interruptedExit));
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
      assert.isFalse(yield* fs.exists(ignoredResidue));
    }),
  );

  it.effect("restores the original state before re-propagating an apply defect", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-defective-apply-rollback-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredResidue = path.join(fixture.target, ".repos/sample/partial.fixture");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      const defect = new Error("simulated apply defect with secret-token-value");
      let rollbackObservedLock = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "rollback") {
          return fs.exists(lockPath).pipe(
            Effect.orDie,
            Effect.tap((exists) =>
              Effect.sync(() => {
                rollbackObservedLock = rollbackObservedLock || exists;
              }),
            ),
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
          );
        }
        if (phase !== "apply") {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        return runGitCommand(commandRoot, plan, phase, args, options).pipe(
          Effect.andThen(fs.writeFileString(ignoredResidue, "partial apply output\n")),
          Effect.orDie,
          Effect.andThen(Effect.die(defect)),
        );
      };

      const exit = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.exit);

      assert.isTrue(Exit.isFailure(exit));
      if (Exit.isSuccess(exit)) {
        return assert.fail("Expected the original apply defect");
      }
      assert.isTrue(Cause.hasDies(exit.cause));
      assert.strictEqual(Cause.squash(exit.cause), defect);
      assert.isTrue(rollbackObservedLock);
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
      assert.equal(
        yield* runGit(fixture.target, [
          "status",
          "--porcelain=v1",
          "--untracked-files=all",
          "--ignored=matching",
          "--",
          fixture.repo.prefix,
        ]),
        "",
      );
      assert.isFalse(yield* fs.exists(ignoredResidue));
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect("reports rollback failure when ignored managed residue survives cleanup", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-real-partial-apply-residue-",
      });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const ignoredPath = path.join(fixture.target, ".repos/sample/partial.fixture");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let failedApply = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "rollback" && args[0] === "clean") {
          return Effect.succeed({ stderr: "", stdout: "" });
        }
        if (phase !== "apply" || failedApply) {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return fs.writeFileString(ignoredPath, "partial apply output\n").pipe(
          Effect.orDie,
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoRollbackError");
      if (error._tag !== "ReferenceRepoRollbackError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.failure, "verification");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* fs.readFileString(ignoredPath), "partial apply output\n");
      assert.isTrue(yield* fs.exists(lockPath));
      assert.notProperty(error, "rootDir");
      assert.notProperty(error, "cause");
    }),
  );

  it.effect("returns an explicit failure when apply rollback cannot restore the target", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-rollback-failure-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let failedApply = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "rollback") {
          return Effect.fail(simulatedSnapshotError(commandRoot, plan, phase));
        }
        if (phase !== "apply" || failedApply) {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        failedApply = true;
        return runGitCommand(commandRoot, plan, phase, args, options).pipe(
          Effect.flatMap(() => Effect.fail(simulatedSnapshotError(commandRoot, plan, phase))),
        );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoRollbackError");
      assert.match(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), /\.repos\/sample/u);
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
      assert.isTrue(yield* fs.exists(lockPath));
      assert.notProperty(error, "rootDir");
      assert.notProperty(error, "cause");
    }),
  );

  it.effect("applies nothing when the second repository fetch or build fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-multi-failure-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const blob = (yield* runGit(fixture.upstream, ["hash-object", "kept.txt"])).trim();
      yield* runGit(fixture.upstream, ["tag", "blob-v1", blob]);
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const secondRepo = { ...fixture.repo, id: "second", prefix: ".repos/second" };

      for (const latestRef of ["missing-ref", "blob-v1"]) {
        const error = yield* syncReferenceRepos({ rootDir: fixture.target, latest: true }, [
          fixture.repo,
          { ...secondRepo, latestRef },
        ]).pipe(Effect.flip);

        assert.equal(error._tag, "ReferenceRepoGitSnapshotError");
        if (error._tag !== "ReferenceRepoGitSnapshotError") {
          return assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.phase, latestRef === "missing-ref" ? "fetch" : "build");
        assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
        assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      }
    }),
  );

  it.effect("skips apply when the second cleanliness check observes a race", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-clean-race-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      for (let index = 0; index < 200; index += 1) {
        yield* writeFixtureFile(
          fixture.upstream,
          `bulk/file-${String(index).padStart(3, "0")}.txt`,
          `${index}\n`,
        );
      }
      yield* commitAll(fixture.upstream, "larger race snapshot");
      yield* runGit(fixture.upstream, ["tag", "v2"]);
      const racePath = path.join(fixture.target, "race.txt");
      const fetchHead = path.join(fixture.target, ".git/FETCH_HEAD");
      const originalTree = (yield* runGit(fixture.target, ["write-tree"])).trim();
      const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
      const raceWriter = yield* spawner.spawn(
        ChildProcess.make("node", [
          "-e",
          'const fs = require("node:fs"); const marker = process.argv[1]; const target = process.argv[2]; const poll = () => { if (fs.existsSync(marker)) { fs.writeFileSync(target, "raced\\n"); return; } setTimeout(poll, 1); }; poll();',
          fetchHead,
          racePath,
        ]),
      );

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id, latest: true },
        [{ ...fixture.repo, latestRef: "v2" }],
      ).pipe(Effect.flip);
      assert.equal(Number(yield* raceWriter.exitCode), 0);

      assert.equal(error._tag, "ReferenceRepoWorkspaceDirtyError");
      assert.equal((yield* runGit(fixture.target, ["write-tree"])).trim(), originalTree);
      assert.equal(yield* fs.readFileString(racePath), "raced\n");
      assert.isTrue(yield* fs.exists(path.join(fixture.target, ".repos/sample/Sql/old.ts")));
      assert.isFalse(yield* fs.exists(path.join(fixture.target, ".repos/sample/SQL/query.ts")));
    }),
  );

  it.effect(
    "rejects a concurrent linked-worktree sync through the shared Git common-directory lock",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-lock-shared-" });
        const fixture = yield* makeSnapshotFixture(rootDir);
        const linkedTarget = path.join(rootDir, "linked-target");
        yield* runGit(fixture.target, ["worktree", "add", "--quiet", "--detach", linkedTarget]);
        const primaryLockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
        const linkedLockPath = yield* getReferenceRepoSyncLockPath(linkedTarget);
        assert.equal(linkedLockPath, primaryLockPath);

        const firstFetchStarted = yield* Deferred.make<void>();
        const releaseFirstFetch = yield* Deferred.make<void>();
        let firstFetchCount = 0;
        let secondFetchCount = 0;
        const firstRunner: ReferenceRepoGitCommandRunner = (
          commandRoot,
          plan,
          phase,
          args,
          options,
        ) => {
          if (phase !== "fetch") {
            return runGitCommand(commandRoot, plan, phase, args, options);
          }
          firstFetchCount += 1;
          return Deferred.succeed(firstFetchStarted, undefined).pipe(
            Effect.andThen(Deferred.await(releaseFirstFetch)),
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
          );
        };
        const secondRunner: ReferenceRepoGitCommandRunner = (
          commandRoot,
          plan,
          phase,
          args,
          options,
        ) => {
          if (phase !== "fetch") {
            return runGitCommand(commandRoot, plan, phase, args, options);
          }
          secondFetchCount += 1;
          return Effect.fail(simulatedSnapshotError(commandRoot, plan, phase));
        };

        const firstSync = yield* syncReferenceRepos(
          { rootDir: fixture.target, repoId: fixture.repo.id },
          [fixture.repo],
          firstRunner,
        ).pipe(Effect.forkChild({ startImmediately: true }));
        yield* Deferred.await(firstFetchStarted);
        assert.isTrue(yield* fs.exists(primaryLockPath));

        const error = yield* syncReferenceRepos(
          { rootDir: linkedTarget, repoId: fixture.repo.id },
          [fixture.repo],
          secondRunner,
        ).pipe(Effect.flip);

        assert.equal(error._tag, "ReferenceRepoSyncBusyError");
        assert.isTrue(isReferenceRepoSyncError(error));
        assert.equal(firstFetchCount, 1);
        assert.equal(secondFetchCount, 0);
        assert.notProperty(error, "rootDir");
        assert.notProperty(error, "lockPath");
        assert.notProperty(error, "cause");

        yield* Deferred.succeed(releaseFirstFetch, undefined);
        yield* Fiber.join(firstSync);
        assert.isFalse(yield* fs.exists(primaryLockPath));
        assert.equal(yield* runGit(linkedTarget, ["status", "--porcelain=v1"]), "");
      }),
  );

  it.effect("does not steal or remove a pre-existing sync lock", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-lock-busy-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      yield* fs.writeFileString(lockPath, "pre-existing lock\n");
      let fetched = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase === "fetch") {
          fetched = true;
        }
        return runGitCommand(commandRoot, plan, phase, args, options);
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoSyncBusyError");
      assert.isTrue(isReferenceRepoSyncError(error));
      assert.isFalse(fetched);
      assert.isTrue(yield* fs.exists(lockPath));
      assert.notProperty(error, "rootDir");
      assert.notProperty(error, "lockPath");
      assert.notProperty(error, "cause");
    }),
  );

  it.effect("releases the sync lock when in-flight work is interrupted", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-lock-interrupt-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      const fetchStarted = yield* Deferred.make<void>();
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) =>
        phase === "fetch"
          ? Deferred.succeed(fetchStarted, undefined).pipe(Effect.andThen(Effect.never))
          : runGitCommand(commandRoot, plan, phase, args, options);
      const sync = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(fetchStarted);
      assert.isTrue(yield* fs.exists(lockPath));
      yield* Fiber.interrupt(sync);
      assert.isFalse(yield* fs.exists(lockPath));
      assert.equal(yield* runGit(fixture.target, ["status", "--porcelain=v1"]), "");
    }),
  );

  it.effect("reports a safe typed error when sync lock release fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-real-lock-release-" });
      const fixture = yield* makeSnapshotFixture(rootDir);
      const lockPath = yield* getReferenceRepoSyncLockPath(fixture.target);
      let replacedLock = false;
      const commandRunner: ReferenceRepoGitCommandRunner = (
        commandRoot,
        plan,
        phase,
        args,
        options,
      ) => {
        if (phase !== "fetch" || replacedLock) {
          return runGitCommand(commandRoot, plan, phase, args, options);
        }
        replacedLock = true;
        return fs
          .remove(lockPath)
          .pipe(
            Effect.andThen(fs.makeDirectory(lockPath)),
            Effect.orDie,
            Effect.andThen(runGitCommand(commandRoot, plan, phase, args, options)),
          );
      };

      const error = yield* syncReferenceRepos(
        { rootDir: fixture.target, repoId: fixture.repo.id },
        [fixture.repo],
        commandRunner,
      ).pipe(Effect.flip);

      assert.equal(error._tag, "ReferenceRepoSyncLockError");
      if (error._tag !== "ReferenceRepoSyncLockError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "release");
      assert.equal(error.failure, "filesystem");
      assert.isTrue(isReferenceRepoSyncError(error));
      assert.isTrue(yield* fs.exists(lockPath));
      assert.notProperty(error, "rootDir");
      assert.notProperty(error, "lockPath");
      assert.notProperty(error, "cause");
    }),
  );

  it.effect("rejects unknown repo selectors", () =>
    Effect.gen(function* () {
      const error = yield* syncReferenceRepos({
        repoId: "missing",
        dryRun: true,
      }).pipe(Effect.flip);

      if (error._tag !== "ReferenceRepoSelectionError") {
        assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.repoId, "missing");
      assert.deepStrictEqual(error.expectedRepoIds, ["effect-smol", "alchemy-effect"]);
      assert.ok(!("cause" in error));
      assert.equal(
        error.message,
        'Unknown reference repo "missing". Expected one of: effect-smol, alchemy-effect.',
      );
      assert.isTrue(isReferenceRepoSyncError(error));
    }),
  );

  it.effect("reports non-zero git exits without retaining process output", () => {
    const commands: Array<{ readonly command: string; readonly args: ReadonlyArray<string> }> = [];

    return Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({
        prefix: "sync-reference-repos-exit-error-",
      });
      yield* fs.writeFileString(
        path.join(rootDir, "pnpm-workspace.yaml"),
        "catalog:\n  effect: 4.0.0-beta.73\n",
      );

      const error = yield* syncReferenceRepos({ rootDir, repoId: "effect-smol" }).pipe(
        Effect.provide(
          mockSpawnerLayer(
            commands,
            mockHandle({ exitCode: 23, stderr: "snapshot failed secret-token-value\n" }),
          ),
        ),
        Effect.flip,
      );

      if (error._tag !== "ReferenceRepoGitSnapshotError") {
        assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "exit");
      assert.equal(error.phase, "verify-clean");
      assert.equal(error.repoId, effectSmol.id);
      assert.equal(error.action, "replace");
      assert.equal(error.repository, effectSmol.repository);
      assert.equal(error.ref, "effect@4.0.0-beta.73");
      assert.equal(error.rootDir, rootDir);
      assert.equal(error.argumentCount, commands[0]?.args.length);
      assert.equal(error.exitCode, 23);
      assert.equal(error.stdoutLength, 5);
      assert.equal(error.stderrLength, 35);
      assert.notProperty(error, "args");
      assert.notProperty(error, "stderr");
      assert.notInclude(error.message, "secret-token-value");
      assert.ok(!("cause" in error));
      assert.equal(
        error.message,
        'Git snapshot replace for reference repo "effect-smol" failed during verify-clean exit.',
      );
    });
  });

  it.effect("dry-runs every configured repository from latest refs without spawning git", () => {
    let spawned = false;
    return Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-all-dry-" });
      const plans = yield* syncReferenceRepos({ rootDir, latest: true, dryRun: true }).pipe(
        Effect.provide(
          Layer.succeed(
            ChildProcessSpawner.ChildProcessSpawner,
            ChildProcessSpawner.make(() => {
              spawned = true;
              return Effect.die("dry run spawned git");
            }),
          ),
        ),
      );

      assert.isFalse(spawned);
      assert.deepStrictEqual(
        plans.map(({ repo, ref }) => [repo.id, ref]),
        [
          ["effect-smol", "main"],
          ["alchemy-effect", "main"],
        ],
      );
    });
  });

  it.effect("maps public CLI flags into a dry-run sync plan", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-cli-" });
      yield* Command.runWith(syncReferenceReposCommand, { version: "0.0.0" })([
        "--repo",
        "effect-smol",
        "--latest",
        "--root",
        rootDir,
        "--dry-run",
      ]);
      assert.isFalse(yield* fs.exists(path.join(rootDir, effectSmol.prefix)));
    }),
  );

  it.effect("resolves an omitted root from process.cwd during version planning", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const versionSourcePath = "missing-default-root-version.json";
      const error = yield* syncReferenceRepos({ repoId: "effect-smol", dryRun: true }, [
        { ...effectSmol, versionSourcePath },
      ]).pipe(Effect.flip);

      if (error._tag !== "ReferenceRepoVersionSourceError") {
        return assert.fail(`Unexpected error: ${error._tag}`);
      }
      assert.equal(error.operation, "read");
      assert.equal(error.sourcePath, path.resolve(process.cwd(), versionSourcePath));
    }),
  );

  it.effect("accepts whitespace-only stdout from a successful Git command", () => {
    const commands: Array<{ readonly command: string; readonly args: ReadonlyArray<string> }> = [];
    return Effect.gen(function* () {
      const plan = yield* planReferenceRepoSync(effectSmol, process.cwd(), true);
      const result = yield* runGitCommand(process.cwd(), plan, "verify-clean", ["status"], {
        logStdout: true,
      }).pipe(
        Effect.provide(mockSpawnerLayer(commands, mockHandle({ stdout: "   \n" }))),
        Effect.scoped,
      );

      assert.equal(result.stdout, "   \n");
      assert.equal(commands[0]?.command, "git");
    });
  });

  it.effect("maps git spawn and communication failures with safe context", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-errors-" });
      const cause = PlatformError.systemError({
        _tag: "Unknown",
        module: "ChildProcess",
        method: "spawn",
      });
      const spawnError = yield* syncReferenceRepos({
        rootDir,
        repoId: "effect-smol",
        latest: true,
      }).pipe(
        Effect.provide(
          Layer.succeed(
            ChildProcessSpawner.ChildProcessSpawner,
            ChildProcessSpawner.make(() => Effect.fail(cause)),
          ),
        ),
        Effect.flip,
      );
      assert.equal(spawnError._tag, "ReferenceRepoGitSnapshotError");
      if (spawnError._tag !== "ReferenceRepoGitSnapshotError") {
        return assert.fail(`Unexpected error: ${spawnError._tag}`);
      }
      assert.equal(spawnError.operation, "spawn");
      assert.equal(spawnError.phase, "verify-clean");
      assert.strictEqual(spawnError.cause, cause);

      for (const handle of [
        mockHandle({ stdoutError: cause }),
        mockHandle({ stderrError: cause }),
        mockHandle({ exitError: cause }),
      ]) {
        const communicateError = yield* syncReferenceRepos({
          rootDir,
          repoId: "effect-smol",
          latest: true,
        }).pipe(Effect.provide(mockSpawnerLayer([], handle)), Effect.flip);
        assert.equal(communicateError._tag, "ReferenceRepoGitSnapshotError");
        if (communicateError._tag !== "ReferenceRepoGitSnapshotError") {
          return assert.fail(`Unexpected error: ${communicateError._tag}`);
        }
        assert.equal(communicateError.operation, "communicate");
        assert.equal(communicateError.phase, "verify-clean");
        assert.strictEqual(communicateError.cause, cause);
      }
    }),
  );

  it.effect("rejects missing, null, non-string, and empty nested package versions", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const rootDir = yield* fs.makeTempDirectoryScoped({ prefix: "sync-version-shapes-" });
      const repo = {
        ...alchemyEffect,
        versionSourcePath: "version.json",
        packageVersionPath: ["outer", "version"],
      };
      const sourcePath = path.join(rootDir, repo.versionSourcePath);

      for (const source of [
        "{}",
        '{"outer":null}',
        '{"outer":{"version":7}}',
        '{"outer":{"version":""}}',
      ]) {
        yield* fs.writeFileString(sourcePath, source);
        const error = yield* resolveReferenceRepoRef(repo, rootDir, false).pipe(Effect.flip);
        assert.equal(error._tag, "ReferenceRepoVersionResolutionError");
        assert.equal(
          error.message,
          `No version was found for reference repo "${repo.id}" at ${sourcePath}:outer.version.`,
        );
        assert.isTrue(isReferenceRepoSyncError(error));
      }
    }),
  );
});

it("does not launch on import and launches once for direct execution", () => {
  const programs: Array<object> = [];
  const launch = <E, A>(program: Effect.Effect<A, E, never>) => programs.push(program);
  assert.isFalse(runSyncReferenceReposMain(false, launch));
  assert.isTrue(runSyncReferenceReposMain(true, launch));
  assert.lengthOf(programs, 1);
});
