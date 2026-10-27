import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Config from "effect/Config";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import { Command, CliError } from "effect/cli";
import * as TestConsole from "effect/testing/TestConsole";
import { fromJsonStringPretty } from "@coddy/shared/schemaJson";

import {
  ReleaseGitHubOutputConfigurationError,
  ReleaseGitHubOutputWriteError,
  ReleasePackageManifestError,
  releaseCargoLockFile,
  releasePackageFiles,
  releaseRustPackageFiles,
  releaseVersionFiles,
  updateReleasePackageVersions,
  updateReleasePackageVersionsCommand,
  runUpdateReleasePackageVersionsMain,
} from "./update-release-package-versions.ts";

const ScriptTestLayer = NodeServices.layer;
const runCli = Command.runWith(updateReleasePackageVersionsCommand, { version: "0.0.0" });
const PackageJsonSchema = Schema.Record(Schema.String, Schema.Unknown);
const PackageJsonPrettyJson = fromJsonStringPretty(PackageJsonSchema);
const decodePackageJson = Schema.decodeEffect(PackageJsonPrettyJson);
const encodePackageJson = Schema.encodeEffect(PackageJsonPrettyJson);

const makePromiseGate = () => {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
};

const writePackageJsonFixtures = Effect.fn("writePackageJsonFixtures")(function* (
  rootDir: string,
  version: string,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;

  for (const relativePath of releasePackageFiles) {
    const filePath = path.join(rootDir, relativePath);
    yield* fs.makeDirectory(path.dirname(filePath), { recursive: true });
    yield* fs.writeFileString(
      filePath,
      `${yield* encodePackageJson({
        name: relativePath,
        version,
        private: true,
      })}\n`,
    );
  }

  for (const relativePath of releaseRustPackageFiles) {
    const filePath = path.join(rootDir, relativePath);
    yield* fs.makeDirectory(path.dirname(filePath), { recursive: true });
    yield* fs.writeFileString(
      filePath,
      `[package]\nname = "${relativePath.includes("desktop") ? "coddy-studio-desktop" : "coddy-studio-server"}"\nversion = "${version}"\n`,
    );
  }
  yield* fs.writeFileString(
    path.join(rootDir, releaseCargoLockFile),
    `version = 4\n\n[[package]]\nname = "coddy-studio-desktop"\nversion = "${version}"\n\n[[package]]\nname = "coddy-studio-server"\nversion = "${version}"\n`,
  );
});

const readReleaseVersions = Effect.fn("readReleaseVersions")(function* (rootDir: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const versions = new Map<string, string>();

  for (const relativePath of releasePackageFiles) {
    const filePath = path.join(rootDir, relativePath);
    const packageJson = yield* fs.readFileString(filePath).pipe(Effect.flatMap(decodePackageJson));
    versions.set(relativePath, String(packageJson.version));
  }

  return versions;
});

const assertManifestTransactionState = Effect.fn("assertManifestTransactionState")(function* (
  rootDir: string,
  expectedVersion: string,
  transactionId: string,
  recoverableBackups: ReadonlyArray<string> = [],
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const versions = yield* readReleaseVersions(rootDir);

  assert.deepStrictEqual(
    Array.from(versions.values()),
    releasePackageFiles.map(() => expectedVersion),
  );
  for (const relativePath of releasePackageFiles) {
    const filePath = path.join(rootDir, relativePath);
    assert.isTrue(yield* fs.exists(filePath));
    assert.isFalse(yield* fs.exists(`${filePath}.coddy-studio-${transactionId}.stage`));
    assert.equal(
      yield* fs.exists(`${filePath}.coddy-studio-${transactionId}.backup`),
      recoverableBackups.includes(filePath),
    );
  }
});

const captureLogs = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const result = yield* effect;
    const logs = (yield* TestConsole.logLines).filter(
      (line): line is string => typeof line === "string",
    );
    return { result, logs };
  }).pipe(Effect.provide(Layer.fresh(TestConsole.layer)));

it.layer(ScriptTestLayer)("update-release-package-versions", (it) => {
  it.effect("updates all release package versions under the provided root", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-",
      });

      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const result = yield* updateReleasePackageVersions("1.2.3", { rootDir: baseDir });
      const versions = yield* readReleaseVersions(baseDir);

      assert.deepStrictEqual(result, { changed: true });
      assert.deepStrictEqual(
        Array.from(versions.entries()),
        releasePackageFiles.map((relativePath) => [relativePath, "1.2.3"]),
      );
    }),
  );

  it.effect("updates Rust package manifests and lockfile entries", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-rust-versions-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const rustManifestPaths = [
        "apps/server/Cargo.toml",
        "apps/desktop/src-tauri/Cargo.toml",
      ] as const;
      for (const relativePath of rustManifestPaths) {
        const filePath = path.join(baseDir, relativePath);
        yield* fs.makeDirectory(path.dirname(filePath), { recursive: true });
        yield* fs.writeFileString(
          filePath,
          `[package]\nname = "${path.basename(path.dirname(filePath))}"\nversion = "0.0.1"\n`,
        );
      }
      const lockfilePath = path.join(baseDir, "Cargo.lock");
      yield* fs.writeFileString(
        lockfilePath,
        `version = 4\n\n[[package]]\nname = "coddy-studio-desktop"\nversion = "0.0.1"\n\n[[package]]\nname = "coddy-studio-server"\nversion = "0.0.1"\n`,
      );

      yield* updateReleasePackageVersions("1.2.3", { rootDir: baseDir });

      for (const relativePath of rustManifestPaths) {
        assert.include(
          yield* fs.readFileString(path.join(baseDir, relativePath)),
          'version = "1.2.3"',
        );
      }
      const lockfile = yield* fs.readFileString(lockfilePath);
      assert.equal(lockfile.match(/version = "1\.2\.3"/g)?.length, 2);
    }),
  );

  it.effect("preserves indentation, newline style, and final-newline policy", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-format-style-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const fixtures = [
        '{\n  "name": "server",\n  "version": "0.0.1"\n}\n',
        '{\r\n    "name": "desktop",\r\n    "version": "0.0.1"\r\n}',
        '{\n\t"name": "web",\n\t"version": "0.0.1"\n}\n',
        '{"name":"contracts","version":"0.0.1"}',
      ] as const;
      const expected = [
        '{\n  "name": "server",\n  "version": "1.2.3"\n}\n',
        '{\r\n    "name": "desktop",\r\n    "version": "1.2.3"\r\n}',
        '{\n\t"name": "web",\n\t"version": "1.2.3"\n}\n',
        '{"name":"contracts","version":"1.2.3"}',
      ] as const;

      for (const [index, relativePath] of releasePackageFiles.entries()) {
        const filePath = path.join(baseDir, relativePath);
        yield* fs.makeDirectory(path.dirname(filePath), { recursive: true });
        yield* fs.writeFileString(filePath, fixtures[index]!);
      }

      yield* updateReleasePackageVersions("1.2.3", { rootDir: baseDir });

      for (const [index, relativePath] of releasePackageFiles.entries()) {
        assert.equal(yield* fs.readFileString(path.join(baseDir, relativePath)), expected[index]);
      }
    }),
  );

  it.effect("returns changed=false when all versions already match", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-unchanged-",
      });

      yield* writePackageJsonFixtures(baseDir, "1.2.3");

      const result = yield* updateReleasePackageVersions("1.2.3", { rootDir: baseDir });

      assert.deepStrictEqual(result, { changed: false });
    }),
  );

  it.effect("uses process.cwd when no root override is provided", () => {
    const reads: Array<string> = [];
    return Effect.gen(function* () {
      const result = yield* updateReleasePackageVersions("1.2.3").pipe(
        Effect.provideService(
          FileSystem.FileSystem,
          FileSystem.makeNoop({
            readFileString: (filePath) => {
              const resolvedPath = String(filePath);
              reads.push(resolvedPath);
              if (resolvedPath.endsWith("package.json")) {
                return Effect.succeed('{"version":"1.2.3"}');
              }
              if (resolvedPath.endsWith("Cargo.lock")) {
                return Effect.succeed(
                  'version = 4\n\n[[package]]\nname = "coddy-studio-desktop"\nversion = "1.2.3"\n\n[[package]]\nname = "coddy-studio-server"\nversion = "1.2.3"\n',
                );
              }
              return Effect.succeed('[package]\nversion = "1.2.3"\n');
            },
          }),
        ),
      );
      assert.deepStrictEqual(result, { changed: false });
      assert.lengthOf(reads, releaseVersionFiles.length);
      assert.match(reads[0]!, /[\\/]apps[\\/]server[\\/]package\.json$/);
    });
  });

  it.effect("preserves manifest read context and the filesystem cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-read-error-",
      });
      const filePath = path.join(baseDir, releasePackageFiles[0]);

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
      }).pipe(Effect.flip);

      assert.instanceOf(error, ReleasePackageManifestError);
      assert.equal(error.operation, "read");
      assert.equal(error.filePath, filePath);
      assert.instanceOf(error.cause, PlatformError.PlatformError);
      assert.equal(error.message, `Failed to read release package manifest '${filePath}'.`);
    }),
  );

  it.effect("preserves manifest decode context and the schema cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-decode-error-",
      });
      const filePath = path.join(baseDir, releasePackageFiles[0]);

      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      yield* fs.writeFileString(filePath, "not json");

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
      }).pipe(Effect.flip);

      assert.equal(error.operation, "decode");
      assert.equal(error.filePath, filePath);
      assert.isTrue(Schema.isSchemaError(error.cause));
      assert.equal(error.message, `Failed to decode release package manifest '${filePath}'.`);
    }),
  );

  it.effect("preserves manifest encode context and the schema cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-encode-error-",
      });
      const filePath = path.join(baseDir, releasePackageFiles[0]);
      const cyclic: Record<string, unknown> = {};
      cyclic.self = cyclic;
      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        encodePackageJson: () => encodePackageJson(cyclic),
      }).pipe(Effect.flip);

      assert.equal(error.operation, "encode");
      assert.equal(error.filePath, filePath);
      assert.isTrue(Schema.isSchemaError(error.cause));
      assert.equal(error.message, `Failed to encode release package manifest '${filePath}'.`);
    }),
  );

  it.effect("preserves manifest write context and the filesystem cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-write-error-",
      });
      const filePath = path.join(baseDir, releasePackageFiles[0]);
      const writeCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "writeFileString",
        pathOrDescriptor: filePath,
      });

      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        writeFileString: (target, contents, options) =>
          target.endsWith(".stage")
            ? Effect.fail(writeCause)
            : fs.writeFileString(target, contents, options),
      }).pipe(Effect.flip);

      assert.equal(error.operation, "write");
      assert.equal(error.filePath, filePath);
      assert.strictEqual(error.cause, writeCause);
      assert.deepStrictEqual(error.rollbackFailures, []);
      assert.equal(error.message, `Failed to write release package manifest '${filePath}'.`);
    }),
  );

  it.effect("retains the replacement cause when rollback also fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-rollback-failure-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const failedPath = path.join(baseDir, releasePackageFiles[1]);
      const replaceCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: failedPath,
      });
      const rollbackCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: firstPath,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "rollback-failure-test",
        moveFile: (source, target) => {
          if (source.endsWith(".stage") && target === failedPath) {
            return Effect.fail(replaceCause);
          }
          if (source.endsWith(".backup") && target === firstPath) {
            return Effect.fail(rollbackCause);
          }
          return fs.rename(source, target);
        },
      }).pipe(Effect.flip);

      assert.strictEqual(error.cause, replaceCause);
      assert.deepStrictEqual(error.rollbackFailures, [
        { operation: "restore-original", filePath: firstPath, cause: rollbackCause },
      ]);
      for (const relativePath of releasePackageFiles) {
        const filePath = path.join(baseDir, relativePath);
        assert.isTrue(yield* fs.exists(filePath));
        assert.isFalse(yield* fs.exists(`${filePath}.coddy-studio-rollback-failure-test.stage`));
      }
      assert.equal(
        String((yield* decodePackageJson(yield* fs.readFileString(firstPath))).version),
        "1.2.3",
      );
      assert.isTrue(yield* fs.exists(`${firstPath}.coddy-studio-rollback-failure-test.backup`));
      assert.equal(
        String(
          (yield* fs
            .readFileString(`${firstPath}.coddy-studio-rollback-failure-test.backup`)
            .pipe(Effect.flatMap(decodePackageJson))).version,
        ),
        "0.0.1",
      );
    }),
  );

  it.effect("validates every changed manifest before staging any file", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-preflight-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const cyclic: Record<string, unknown> = {};
      cyclic.self = cyclic;
      let encodeCount = 0;
      const writes: Array<string> = [];

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        encodePackageJson: (input) => {
          encodeCount += 1;
          return encodeCount === 2 ? encodePackageJson(cyclic) : encodePackageJson(input);
        },
        writeFileString: (target, contents, options) => {
          writes.push(String(target));
          return fs.writeFileString(target, contents, options);
        },
      }).pipe(Effect.flip);

      assert.equal(error.operation, "encode");
      assert.deepStrictEqual(writes, []);
    }),
  );

  it.effect("rolls back an earlier replacement when a later replacement fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-rollback-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const originals = new Map<string, string>();
      for (const relativePath of releasePackageFiles) {
        originals.set(relativePath, yield* fs.readFileString(path.join(baseDir, relativePath)));
      }
      const failedPath = path.join(baseDir, releasePackageFiles[1]);
      const replaceCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: failedPath,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "rollback-test",
        moveFile: (source, target) =>
          source.endsWith(".stage") && target === failedPath
            ? Effect.fail(replaceCause)
            : fs.rename(source, target),
      }).pipe(Effect.flip);

      assert.equal(error.operation, "replace");
      assert.equal(error.filePath, failedPath);
      assert.strictEqual(error.cause, replaceCause);
      assert.deepStrictEqual(error.rollbackFailures, []);
      for (const relativePath of releasePackageFiles) {
        assert.equal(
          yield* fs.readFileString(path.join(baseDir, relativePath)),
          originals.get(relativePath),
        );
      }
    }),
  );

  it.effect("reports staged-file cleanup failures without hiding the write failure", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-stage-cleanup-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const secondPath = path.join(baseDir, releasePackageFiles[1]);
      const firstStage = `${firstPath}.coddy-studio-stage-cleanup-test.stage`;
      const secondStage = `${secondPath}.coddy-studio-stage-cleanup-test.stage`;
      const writeCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "writeFileString",
        pathOrDescriptor: secondStage,
      });
      const cleanupCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "remove",
        pathOrDescriptor: firstStage,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "stage-cleanup-test",
        writeFileString: (target, contents, options) =>
          target === secondStage
            ? Effect.fail(writeCause)
            : fs.writeFileString(target, contents, options),
        removeFile: (target, options) =>
          target === firstStage
            ? fs.remove(target, options).pipe(Effect.flatMap(() => Effect.fail(cleanupCause)))
            : fs.remove(target, options),
      }).pipe(Effect.flip);

      assert.strictEqual(error.cause, writeCause);
      assert.deepStrictEqual(error.rollbackFailures, [
        { operation: "remove-staged", filePath: firstPath, cause: cleanupCause },
      ]);
      assert.isFalse(yield* fs.exists(firstStage));
    }),
  );

  it.effect("leaves originals untouched when creating the first backup fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-backup-failure-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const original = yield* fs.readFileString(firstPath);
      const backupCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: firstPath,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "backup-failure-test",
        copyFile: (source, target) =>
          source === firstPath ? Effect.fail(backupCause) : fs.copyFile(source, target),
      }).pipe(Effect.flip);

      assert.equal(error.operation, "replace");
      assert.equal(error.filePath, firstPath);
      assert.strictEqual(error.cause, backupCause);
      assert.deepStrictEqual(error.rollbackFailures, []);
      assert.equal(yield* fs.readFileString(firstPath), original);
      for (const relativePath of releasePackageFiles) {
        const filePath = path.join(baseDir, relativePath);
        assert.isFalse(yield* fs.exists(`${filePath}.coddy-studio-backup-failure-test.stage`));
        assert.isFalse(yield* fs.exists(`${filePath}.coddy-studio-backup-failure-test.backup`));
      }
    }),
  );

  it.effect("retains a partial backup when its cleanup fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-partial-backup-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const firstBackup = `${firstPath}.coddy-studio-partial-backup-test.backup`;
      const copyCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "copyFile",
        pathOrDescriptor: firstPath,
      });
      const cleanupCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "remove",
        pathOrDescriptor: firstBackup,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "partial-backup-test",
        copyFile: (source, target) =>
          fs.copyFile(source, target).pipe(Effect.flatMap(() => Effect.fail(copyCause))),
        removeFile: (target, options) =>
          target === firstBackup ? Effect.fail(cleanupCause) : fs.remove(target, options),
      }).pipe(Effect.flip);

      assert.strictEqual(error.cause, copyCause);
      assert.deepStrictEqual(error.rollbackFailures, [
        { operation: "cleanup-backup", filePath: firstPath, cause: cleanupCause },
      ]);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "partial-backup-test", [firstPath]);
    }),
  );

  it.effect("reports backup cleanup failures after committing every manifest", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-backup-cleanup-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const secondPath = path.join(baseDir, releasePackageFiles[1]);
      const firstBackup = `${firstPath}.coddy-studio-backup-cleanup-test.backup`;
      const secondBackup = `${secondPath}.coddy-studio-backup-cleanup-test.backup`;
      const firstCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "remove",
        pathOrDescriptor: firstBackup,
      });
      const secondCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "remove",
        pathOrDescriptor: secondBackup,
      });

      const error = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "backup-cleanup-test",
        removeFile: (target, options) => {
          const cause =
            target === firstBackup ? firstCause : target === secondBackup ? secondCause : undefined;
          return cause
            ? fs.remove(target, options).pipe(Effect.flatMap(() => Effect.fail(cause)))
            : fs.remove(target, options);
        },
      }).pipe(Effect.flip);

      assert.equal(error.operation, "cleanup");
      assert.equal(error.filePath, firstPath);
      assert.strictEqual(error.cause, firstCause);
      assert.deepStrictEqual(error.rollbackFailures, [
        { operation: "cleanup-backup", filePath: secondPath, cause: secondCause },
      ]);
      assert.deepStrictEqual(
        Array.from((yield* readReleaseVersions(baseDir)).values()),
        releasePackageFiles.map(() => "1.2.3"),
      );
    }),
  );

  it.effect("rolls back interruption before the first atomic replacement", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-interrupt-first-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const replacementStarted = yield* Deferred.make<void>();
      const releaseReplacement = yield* Deferred.make<void>();
      let targetExistedAtReplacement = false;

      const fiber = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "interrupt-first",
        moveFile: (source, target) =>
          source.endsWith(".stage")
            ? Effect.gen(function* () {
                targetExistedAtReplacement = yield* fs.exists(target);
                yield* Deferred.succeed(replacementStarted, undefined);
                yield* Deferred.await(releaseReplacement);
                return yield* fs.rename(source, target);
              })
            : fs.rename(source, target),
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(replacementStarted);
      const interruption = yield* Fiber.interrupt(fiber).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseReplacement, undefined);
      yield* Fiber.join(interruption);

      assert.isTrue(targetExistedAtReplacement);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "interrupt-first");
    }),
  );

  it.effect("rolls back interruption during the second atomic replacement", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-interrupt-second-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const secondReplacementStarted = yield* Deferred.make<void>();
      const releaseSecondReplacement = yield* Deferred.make<void>();
      let replacementCount = 0;
      let secondTargetExisted = false;

      const fiber = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "interrupt-second",
        moveFile: (source, target) => {
          if (!source.endsWith(".stage")) return fs.rename(source, target);
          replacementCount += 1;
          if (replacementCount === 1) return fs.rename(source, target);
          return Effect.gen(function* () {
            secondTargetExisted = yield* fs.exists(target);
            yield* Deferred.succeed(secondReplacementStarted, undefined);
            yield* Deferred.await(releaseSecondReplacement);
            return yield* fs.rename(source, target);
          });
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(secondReplacementStarted);
      const interruption = yield* Fiber.interrupt(fiber).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseSecondReplacement, undefined);
      yield* Fiber.join(interruption);

      assert.isTrue(secondTargetExisted);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "interrupt-second");
    }),
  );

  it.effect("keeps rollback uninterruptible once restoration starts", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-interrupt-rollback-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const secondPath = path.join(baseDir, releasePackageFiles[1]);
      const rollbackStarted = yield* Deferred.make<void>();
      const releaseRollback = yield* Deferred.make<void>();
      let targetExistedDuringRollback = false;
      const replaceCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: secondPath,
      });

      const fiber = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "interrupt-rollback",
        moveFile: (source, target) => {
          if (source.endsWith(".stage") && target === secondPath) {
            return Effect.fail(replaceCause);
          }
          if (source.endsWith(".backup") && target === firstPath) {
            return Effect.gen(function* () {
              targetExistedDuringRollback = yield* fs.exists(target);
              yield* Deferred.succeed(rollbackStarted, undefined);
              yield* Deferred.await(releaseRollback);
              yield* fs.rename(source, target);
            });
          }
          return fs.rename(source, target);
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(rollbackStarted);
      const interruption = yield* Fiber.interrupt(fiber).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseRollback, undefined);
      yield* Fiber.join(interruption);

      assert.isTrue(targetExistedDuringRollback);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "interrupt-rollback");
    }),
  );

  it.effect("finishes committed-state cleanup when interrupted", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-interrupt-cleanup-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const firstBackup = `${firstPath}.coddy-studio-interrupt-cleanup.backup`;
      const cleanupStarted = yield* Deferred.make<void>();
      const releaseCleanup = yield* Deferred.make<void>();
      let firstCleanupAttempt = true;

      const fiber = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "interrupt-cleanup",
        removeFile: (target, options) => {
          if (target === firstBackup && firstCleanupAttempt) {
            firstCleanupAttempt = false;
            return Deferred.succeed(cleanupStarted, undefined).pipe(
              Effect.andThen(Deferred.await(releaseCleanup)),
              Effect.andThen(fs.remove(target, options)),
            );
          }
          return fs.remove(target, options);
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(cleanupStarted);
      const interruption = yield* Fiber.interrupt(fiber).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseCleanup, undefined);
      yield* Fiber.join(interruption);

      yield* assertManifestTransactionState(baseDir, "1.2.3", "interrupt-cleanup");
    }),
  );

  it.effect("waits for a late stage write before interruption cleanup", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-late-stage-write-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const firstStage = `${firstPath}.coddy-studio-late-stage-write.stage`;
      const callbackStarted = makePromiseGate();
      const interruptionFinished = yield* Deferred.make<void>();
      const callbackRelease = makePromiseGate();
      const callbackSettled = makePromiseGate();
      const artifacts = new Set<string>();
      let firstWrite = true;
      const unexpectedWriteCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "writeFileString",
        pathOrDescriptor: firstStage,
      });

      const transaction = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "late-stage-write",
        removeFile: (target) =>
          Effect.sync(() => {
            artifacts.delete(target);
          }),
        writeFileString: (target) => {
          if (firstWrite) {
            firstWrite = false;
            return Effect.tryPromise({
              try: async () => {
                callbackStarted.release();
                await callbackRelease.promise;
                try {
                  artifacts.add(target);
                } finally {
                  callbackSettled.release();
                }
              },
              catch: () => unexpectedWriteCause,
            });
          }
          return Effect.sync(() => {
            artifacts.add(target);
          });
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Effect.promise(() => callbackStarted.promise);
      const interruption = yield* Fiber.interrupt(transaction).pipe(
        Effect.ensuring(Deferred.succeed(interruptionFinished, undefined)),
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      const interruptionCompletedBeforeCallback = yield* Deferred.isDone(interruptionFinished);

      callbackRelease.release();
      yield* Effect.promise(() => callbackSettled.promise);
      yield* Fiber.join(interruption);

      assert.isFalse(interruptionCompletedBeforeCallback);
      assert.deepStrictEqual(Array.from(artifacts), []);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "late-stage-write");
    }),
  );

  it.effect("waits for a late backup copy before interruption cleanup", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-late-backup-copy-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const callbackStarted = makePromiseGate();
      const interruptionFinished = yield* Deferred.make<void>();
      const callbackRelease = makePromiseGate();
      const callbackSettled = makePromiseGate();
      const artifacts = new Set<string>();
      let firstCopy = true;
      const unexpectedCopyCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "copyFile",
        pathOrDescriptor: firstPath,
      });

      const transaction = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "late-backup-copy",
        removeFile: (target) =>
          Effect.sync(() => {
            artifacts.delete(target);
          }),
        writeFileString: (target) =>
          Effect.sync(() => {
            artifacts.add(target);
          }),
        copyFile: (_source, target) => {
          if (firstCopy) {
            firstCopy = false;
            return Effect.tryPromise({
              try: async () => {
                callbackStarted.release();
                await callbackRelease.promise;
                try {
                  artifacts.add(target);
                } finally {
                  callbackSettled.release();
                }
              },
              catch: () => unexpectedCopyCause,
            });
          }
          return Effect.sync(() => {
            artifacts.add(target);
          });
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Effect.promise(() => callbackStarted.promise);
      const interruption = yield* Fiber.interrupt(transaction).pipe(
        Effect.ensuring(Deferred.succeed(interruptionFinished, undefined)),
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      const interruptionCompletedBeforeCallback = yield* Deferred.isDone(interruptionFinished);

      callbackRelease.release();
      yield* Effect.promise(() => callbackSettled.promise);
      yield* Fiber.join(interruption);

      assert.isFalse(interruptionCompletedBeforeCallback);
      assert.deepStrictEqual(Array.from(artifacts), []);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "late-backup-copy");
    }),
  );

  it.effect("preserves a late backup-copy failure after interruption", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-late-copy-failure-" });
      yield* writePackageJsonFixtures(baseDir, "0.0.1");
      const firstPath = path.join(baseDir, releasePackageFiles[0]);
      const callbackStarted = makePromiseGate();
      const interruptionFinished = yield* Deferred.make<void>();
      const callbackRelease = makePromiseGate();
      const callbackSettled = makePromiseGate();
      const artifacts = new Set<string>();
      let firstCopy = true;
      const copyCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "copyFile",
        pathOrDescriptor: firstPath,
      });

      const transaction = yield* updateReleasePackageVersions("1.2.3", {
        rootDir: baseDir,
        transactionId: () => "late-copy-failure",
        removeFile: (target) =>
          Effect.sync(() => {
            artifacts.delete(target);
          }),
        writeFileString: (target) =>
          Effect.sync(() => {
            artifacts.add(target);
          }),
        copyFile: (_source, target) => {
          if (firstCopy) {
            firstCopy = false;
            return Effect.tryPromise({
              try: async () => {
                callbackStarted.release();
                await callbackRelease.promise;
                try {
                  artifacts.add(target);
                  throw new Error("late copy callback failed");
                } finally {
                  callbackSettled.release();
                }
              },
              catch: () => copyCause,
            });
          }
          return Effect.sync(() => {
            artifacts.add(target);
          });
        },
      }).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Effect.promise(() => callbackStarted.promise);
      const interruption = yield* Fiber.interrupt(transaction).pipe(
        Effect.ensuring(Deferred.succeed(interruptionFinished, undefined)),
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      const interruptionCompletedBeforeCallback = yield* Deferred.isDone(interruptionFinished);

      callbackRelease.release();
      yield* Effect.promise(() => callbackSettled.promise);
      yield* Fiber.join(interruption);
      const transactionExit = yield* Fiber.await(transaction);
      const error = Exit.findErrorOption(transactionExit);

      assert.isFalse(interruptionCompletedBeforeCallback);
      assert.isTrue(Option.isSome(error));
      if (Option.isSome(error)) {
        assert.instanceOf(error.value, ReleasePackageManifestError);
        assert.strictEqual(error.value.cause, copyCause);
      }
      assert.deepStrictEqual(Array.from(artifacts), []);
      yield* assertManifestTransactionState(baseDir, "0.0.1", "late-copy-failure");
    }),
  );

  it.effect("accepts flags before the version positional and appends changed output", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-cli-",
      });
      const githubOutputPath = path.join(baseDir, "github-output.txt");

      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      yield* runCli(["--github-output", "--root", baseDir, "2.0.0"]).pipe(
        Effect.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                GITHUB_OUTPUT: githubOutputPath,
              },
            }),
          ),
        ),
      );

      const githubOutput = yield* fs.readFileString(githubOutputPath);
      assert.equal(githubOutput, "changed=true\n");
    }),
  );

  it.effect("logs when nothing changed", () =>
    captureLogs(
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const baseDir = yield* fs.makeTempDirectoryScoped({
          prefix: "update-release-package-versions-cli-log-",
        });

        yield* writePackageJsonFixtures(baseDir, "3.0.0");
        yield* runCli(["3.0.0", "--root", baseDir]);
      }),
    ).pipe(
      Effect.tap(({ logs }) => {
        assert.deepStrictEqual(logs, [
          "All release package versions already match release version.",
        ]);
        return Effect.void;
      }),
    ),
  );

  it.effect("appends changed=false when unchanged output is requested", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "release-unchanged-output-" });
      const outputPath = path.join(baseDir, "output.txt");
      yield* writePackageJsonFixtures(baseDir, "3.0.0");
      yield* runCli(["3.0.0", "--root", baseDir, "--github-output"]).pipe(
        Effect.provide(
          ConfigProvider.layer(ConfigProvider.fromEnv({ env: { GITHUB_OUTPUT: outputPath } })),
        ),
      );
      assert.equal(yield* fs.readFileString(outputPath), "changed=false\n");
    }),
  );

  it.effect("requires GITHUB_OUTPUT when --github-output is set", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-cli-missing-output-",
      });

      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const error = yield* runCli(["4.0.0", "--root", baseDir, "--github-output"]).pipe(
        Effect.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }))),
        Effect.flip,
      );

      assert.instanceOf(error, ReleaseGitHubOutputConfigurationError);
      assert.instanceOf(error.cause, Config.ConfigError);
      assert.equal(
        error.message,
        "Failed to resolve GITHUB_OUTPUT for release package version output.",
      );
    }),
  );

  it.effect("preserves GITHUB_OUTPUT write context and the filesystem cause", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "update-release-package-versions-cli-output-error-",
      });

      yield* writePackageJsonFixtures(baseDir, "0.0.1");

      const error = yield* runCli(["4.0.0", "--root", baseDir, "--github-output"]).pipe(
        Effect.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                GITHUB_OUTPUT: baseDir,
              },
            }),
          ),
        ),
        Effect.flip,
      );

      assert.instanceOf(error, ReleaseGitHubOutputWriteError);
      assert.equal(error.filePath, baseDir);
      assert.instanceOf(error.cause, PlatformError.PlatformError);
      assert.equal(
        error.message,
        `Failed to append release package version output to '${baseDir}'.`,
      );
    }),
  );

  it.effect("rejects unknown flags during cli parsing", () =>
    Effect.gen(function* () {
      const error = yield* runCli(["1.2.3", "--unknown"]).pipe(Effect.flip);

      if (!CliError.isCliError(error)) {
        assert.fail(`Expected CliError, got ${String(error)}`);
      }

      const optionError =
        error._tag === "ShowHelp" ? (error.errors[0] as CliError.CliError | undefined) : error;

      if (!optionError || optionError._tag !== "UnrecognizedOption") {
        assert.fail(`Expected UnrecognizedOption, got ${String(optionError?._tag)}`);
      }

      assert.equal(optionError.option, "--unknown");
    }),
  );

  it.effect("rejects a missing version positional during cli parsing", () =>
    Effect.gen(function* () {
      const error = yield* runCli(["--github-output"]).pipe(Effect.flip);

      if (!CliError.isCliError(error)) {
        assert.fail(`Expected CliError, got ${String(error)}`);
      }

      const versionError =
        error._tag === "ShowHelp" ? (error.errors[0] as CliError.CliError | undefined) : error;

      if (!versionError || versionError._tag !== "MissingArgument") {
        assert.fail(`Expected MissingArgument, got ${String(versionError?._tag)}`);
      }

      assert.equal(versionError.argument, "version");
    }),
  );
});

it("does not launch on import and launches once for direct execution", () => {
  const programs: Array<object> = [];
  const launch = <E, A>(program: Effect.Effect<A, E, never>) => programs.push(program);
  assert.isFalse(runUpdateReleasePackageVersionsMain(false, launch));
  assert.isTrue(runUpdateReleasePackageVersionsMain(true, launch));
  assert.lengthOf(programs, 1);
});
